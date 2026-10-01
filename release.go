package main

import (
	"archive/tar"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"strings"
)

type release struct {
	id     int64
	url    string
	sha256 string
}

func resolveRelease(c channel) (release, error) {
	response, err := get(c.apiURL())
	if err != nil {
		return release{}, err
	}
	defer response.Body.Close()
	var body struct {
		ID     int64 `json:"id"`
		Assets []struct {
			Name   string `json:"name"`
			URL    string `json:"browser_download_url"`
			Digest string `json:"digest"`
		} `json:"assets"`
	}
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		return release{}, fmt.Errorf("unexpected release metadata from %s: %w", c.apiURL(), err)
	}
	for _, asset := range body.Assets {
		if asset.Name != assetName {
			continue
		}
		digest, ok := strings.CutPrefix(asset.Digest, "sha256:")
		if !ok {
			return release{}, fmt.Errorf("%s release has no SHA-256 digest for %s", c, assetName)
		}
		return release{id: body.ID, url: asset.URL, sha256: digest}, nil
	}
	return release{}, fmt.Errorf("%s release has no asset %s", c, assetName)
}

func get(url string) (*http.Response, error) {
	if !strings.HasPrefix(url, "https://") {
		return nil, fmt.Errorf("refusing non-HTTPS URL %s", url)
	}
	response, err := client.Get(url)
	if err != nil {
		return nil, err
	}
	if response.StatusCode != http.StatusOK {
		response.Body.Close()
		return nil, fmt.Errorf("GET %s: %s", url, response.Status)
	}
	return response, nil
}

type progress struct {
	total, done int64
}

func (p *progress) Write(b []byte) (int, error) {
	p.done += int64(len(b))
	fmt.Fprintf(os.Stderr, "\r%3d%%", p.done*100/p.total)
	return len(b), nil
}

func download(url, destination string) (string, error) {
	response, err := get(url)
	if err != nil {
		return "", err
	}
	defer response.Body.Close()
	file, err := os.Create(destination)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	writers := []io.Writer{file, hash}
	if info, err := os.Stderr.Stat(); err == nil && info.Mode()&os.ModeCharDevice != 0 && response.ContentLength > 0 {
		writers = append(writers, &progress{total: response.ContentLength})
		defer fmt.Fprintln(os.Stderr)
	}
	if _, err := io.Copy(io.MultiWriter(writers...), response.Body); err != nil {
		return "", fmt.Errorf("downloading %s: %w", url, err)
	}
	return hex.EncodeToString(hash.Sum(nil)), file.Close()
}

func fetch(r release, staging, target string) (string, error) {
	os.RemoveAll(staging)
	if err := os.Mkdir(staging, 0o755); err != nil {
		return "", err
	}
	defer os.RemoveAll(staging)
	archive := filepath.Join(staging, assetName)
	actual, err := download(r.url, archive)
	if err != nil {
		return "", err
	}
	if actual != r.sha256 {
		return "", fmt.Errorf("SHA-256 mismatch for %s: expected %s, got %s", archive, r.sha256, actual)
	}
	extracted := filepath.Join(staging, "extracted")
	if err := os.Mkdir(extracted, 0o755); err != nil {
		return "", err
	}
	if err := extract(archive, extracted); err != nil {
		return "", fmt.Errorf("extracting %s: %w", archive, err)
	}
	version, err := nvimVersion(extracted)
	if err != nil {
		return "", err
	}
	return version, os.Rename(extracted, target)
}

// extract strips the archive's top-level directory, like tar --strip-components=1.
func extract(archive, dir string) error {
	file, err := os.Open(archive)
	if err != nil {
		return err
	}
	defer file.Close()
	unzipped, err := gzip.NewReader(file)
	if err != nil {
		return err
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return err
	}
	defer root.Close()
	reader := tar.NewReader(unzipped)
	for {
		header, err := reader.Next()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		_, name, found := strings.Cut(path.Clean(header.Name), "/")
		if !found {
			continue
		}
		mode := header.FileInfo().Mode().Perm()
		switch header.Typeflag {
		case tar.TypeDir:
			err = root.MkdirAll(name, mode)
		case tar.TypeReg:
			err = writeFile(root, name, mode, reader)
		default:
			err = fmt.Errorf("unsupported entry %s", header.Name)
		}
		if err != nil {
			return err
		}
	}
}

func writeFile(root *os.Root, name string, mode fs.FileMode, content io.Reader) error {
	if err := root.MkdirAll(path.Dir(name), 0o755); err != nil {
		return err
	}
	file, err := root.OpenFile(name, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
	if err != nil {
		return err
	}
	defer file.Close()
	if _, err := io.Copy(file, content); err != nil {
		return err
	}
	return file.Close()
}
