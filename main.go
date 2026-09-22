package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"time"
)

const assetName = "nvim-linux-x86_64.tar.gz"
const nvimLinkTarget = "../share/nv/active/bin/nvim"
const usage = `Usage:
  nv install stable|nightly
  nv use stable|nightly
  nv update [stable|nightly]
  nv remove [stable|nightly]
  nv rollback stable|nightly
  nv status
  nv help`

var (
	stdout  io.Writer = os.Stdout
	apiBase           = "https://api.github.com"
	client            = &http.Client{
		Timeout: 10 * time.Minute,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) >= 10 {
				return errors.New("stopped after 10 redirects")
			}
			if req.URL.Scheme != "https" {
				return fmt.Errorf("refusing redirect to non-HTTPS URL %s", req.URL)
			}
			return nil
		},
	}
)

type channel string

const (
	stable  channel = "stable"
	nightly channel = "nightly"
)

var channels = []channel{stable, nightly}

func parseChannel(value string) (channel, error) {
	if c := channel(value); slices.Contains(channels, c) {
		return c, nil
	}
	return "", fmt.Errorf("unsupported channel '%s'; expected stable or nightly", value)
}

func (c channel) apiURL() string {
	if c == stable {
		return apiBase + "/repos/neovim/neovim/releases/latest"
	}
	return apiBase + "/repos/neovim/neovim/releases/tags/nightly"
}

type paths struct {
	installs, channels, staging, active, nvimLink string
}

func newPaths() (paths, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return paths{}, err
	}
	state := filepath.Join(home, ".local/share/nv")
	p := paths{
		installs: filepath.Join(state, "installs"),
		channels: filepath.Join(state, "channels"),
		staging:  filepath.Join(state, "staging"),
		active:   filepath.Join(state, "active"),
		nvimLink: filepath.Join(home, ".local/bin/nvim"),
	}
	for _, dir := range []string{
		p.installs,
		filepath.Join(p.channels, "stable"),
		filepath.Join(p.channels, "nightly"),
		filepath.Dir(p.nvimLink),
	} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return paths{}, err
		}
	}
	return p, nil
}

func (p paths) channelLink(c channel, name string) string {
	return filepath.Join(p.channels, string(c), name)
}

type release struct {
	id     int64
	url    string
	sha256 string
}

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintf(os.Stderr, "nv: %s\n", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 1 && slices.Contains([]string{"help", "--help", "-h"}, args[0]) {
		fmt.Fprintln(stdout, usage)
		return nil
	}
	p, err := newPaths()
	if err != nil {
		return err
	}
	if len(args) == 1 {
		switch args[0] {
		case "update":
			return update(p, "")
		case "remove":
			return remove(p, "")
		case "status":
			return status(p)
		}
	} else if len(args) == 2 && slices.Contains([]string{"install", "use", "update", "remove", "rollback"}, args[0]) {
		c, err := parseChannel(args[1])
		if err != nil {
			return err
		}
		switch args[0] {
		case "install":
			return install(p, c)
		case "use":
			if err := install(p, c); err != nil {
				return err
			}
			return activate(p, c)
		case "update":
			return update(p, c)
		case "remove":
			return remove(p, c)
		case "rollback":
			return rollback(p, c)
		}
	}
	return fmt.Errorf("invalid arguments\n\n%s", usage)
}

func install(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	os.RemoveAll(p.staging)
	if err := os.Mkdir(p.staging, 0o755); err != nil {
		return err
	}
	r, err := resolveRelease(c)
	if err != nil {
		return err
	}
	name := fmt.Sprintf("%s-%d", c, r.id)
	target := filepath.Join(p.installs, name)
	if current == name {
		version, err := nvimVersion(target)
		if err != nil {
			return err
		}
		fmt.Fprintf(stdout, "%s is already current: %s (release %d)\n", c, version, r.id)
	} else {
		var version string
		if _, err = os.Stat(target); err == nil {
			version, err = nvimVersion(target)
		} else {
			version, err = fetch(r, p.staging, target)
		}
		if err != nil {
			return err
		}
		if current != "" {
			if err := p.writePointer(c, "previous", current); err != nil {
				return err
			}
		}
		if err := p.writePointer(c, "current", name); err != nil {
			return err
		}
		if err := cleanup(p); err != nil {
			return err
		}
		fmt.Fprintf(stdout, "installed %s %s (release %d)\n", c, version, r.id)
	}
	return os.RemoveAll(p.staging)
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
		sha256, ok := strings.CutPrefix(asset.Digest, "sha256:")
		if !ok {
			return release{}, fmt.Errorf("%s release has no SHA-256 digest for %s", c, assetName)
		}
		return release{id: body.ID, url: asset.URL, sha256: sha256}, nil
	}
	return release{}, fmt.Errorf("%s release has no asset %s", c, assetName)
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

// Strips the archive's top-level directory, like tar --strip-components=1.
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
		case tar.TypeSymlink:
			err = root.Symlink(header.Linkname, name)
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

func nvimVersion(install string) (string, error) {
	output, err := exec.Command(filepath.Join(install, "bin/nvim"), "--version").Output()
	if exit, ok := errors.AsType[*exec.ExitError](err); ok {
		err = fmt.Errorf("%w: %s", err, bytes.TrimSpace(exit.Stderr))
	}
	if err != nil {
		return "", fmt.Errorf("nvim --version in %s: %w", install, err)
	}
	version, _, _ := strings.Cut(string(output), "\n")
	return version, nil
}

func releaseID(install string) string {
	_, id, _ := strings.Cut(install, "-")
	return id
}

func (p paths) readPointer(c channel, name string) (string, error) {
	target, err := os.Readlink(p.channelLink(c, name))
	if errors.Is(err, fs.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	return filepath.Base(target), nil
}

func (p paths) writePointer(c channel, name, install string) error {
	return replaceLink(p.channelLink(c, name), filepath.Join("../../installs", install))
}

func replaceLink(link, target string) error {
	temporary := link + ".new"
	os.Remove(temporary)
	if err := os.Symlink(target, temporary); err != nil {
		return err
	}
	return os.Rename(temporary, link)
}

func removeLink(link string) error {
	if err := os.Remove(link); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}

func activate(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	if current == "" {
		return fmt.Errorf("%s is not installed", c)
	}
	target, err := os.Readlink(p.nvimLink)
	if !(err == nil && target == nvimLinkTarget || errors.Is(err, fs.ErrNotExist)) {
		return fmt.Errorf("%s is not managed by nv; refusing to replace it", p.nvimLink)
	}
	if err := replaceLink(p.active, activeTarget(c)); err != nil {
		return err
	}
	if err := replaceLink(p.nvimLink, nvimLinkTarget); err != nil {
		return err
	}
	version, err := nvimVersion(filepath.Join(p.installs, current))
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "using %s %s (release %s)\n", c, version, releaseID(current))
	return nil
}

func activeTarget(c channel) string {
	return filepath.Join("channels", string(c), "current")
}

func activeChannel(p paths) channel {
	target, err := os.Readlink(p.active)
	if err != nil {
		return ""
	}
	for _, c := range channels {
		if target == activeTarget(c) {
			return c
		}
	}
	return ""
}

func installed(p paths, selection channel) ([]channel, error) {
	var result []channel
	for _, c := range channels {
		if selection != "" && selection != c {
			continue
		}
		current, err := p.readPointer(c, "current")
		if err != nil {
			return nil, err
		}
		if current != "" {
			result = append(result, c)
		}
	}
	if selection != "" && len(result) == 0 {
		return nil, fmt.Errorf("%s is not installed", selection)
	}
	return result, nil
}

func update(p paths, selection channel) error {
	selected, err := installed(p, selection)
	if err != nil {
		return err
	}
	if len(selected) == 0 {
		return errors.New("no channels are installed")
	}
	for _, c := range selected {
		if err := install(p, c); err != nil {
			return err
		}
	}
	return nil
}

func remove(p paths, selection channel) error {
	selected, err := installed(p, selection)
	if err != nil {
		return err
	}
	if active := activeChannel(p); active != "" && slices.Contains(selected, active) {
		if err := removeLink(p.active); err != nil {
			return err
		}
		if err := removeLink(p.nvimLink); err != nil {
			return err
		}
	}
	for _, c := range selected {
		if err := removeLink(p.channelLink(c, "current")); err != nil {
			return err
		}
		if err := removeLink(p.channelLink(c, "previous")); err != nil {
			return err
		}
		fmt.Fprintf(stdout, "removed %s\n", c)
	}
	return cleanup(p)
}

func rollback(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	if current == "" {
		return fmt.Errorf("%s is not installed", c)
	}
	previous, err := p.readPointer(c, "previous")
	if err != nil {
		return err
	}
	if previous == "" {
		return fmt.Errorf("%s has no previous installation", c)
	}
	if err := p.writePointer(c, "previous", current); err != nil {
		return err
	}
	if err := p.writePointer(c, "current", previous); err != nil {
		return err
	}
	version, err := nvimVersion(filepath.Join(p.installs, previous))
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "rolled back %s to %s (release %s)\n", c, version, releaseID(previous))
	return nil
}

func cleanup(p paths) error {
	var referenced []string
	for _, c := range channels {
		for _, name := range []string{"current", "previous"} {
			install, err := p.readPointer(c, name)
			if err != nil {
				return err
			}
			referenced = append(referenced, install)
		}
	}
	entries, err := os.ReadDir(p.installs)
	if err != nil {
		return err
	}
	for _, entry := range entries {
		if !slices.Contains(referenced, entry.Name()) {
			if err := os.RemoveAll(filepath.Join(p.installs, entry.Name())); err != nil {
				return err
			}
		}
	}
	return nil
}

func status(p paths) error {
	active := "none"
	if c := activeChannel(p); c != "" {
		active = string(c)
	}
	fmt.Fprintf(stdout, "active: %s\n", active)
	for _, c := range channels {
		for _, name := range []string{"current", "previous"} {
			install, err := p.readPointer(c, name)
			if err != nil {
				return err
			}
			entry := "none"
			if install != "" {
				version, err := nvimVersion(filepath.Join(p.installs, install))
				if err != nil {
					version = "unknown"
				}
				entry = fmt.Sprintf("release=%s version=%s", releaseID(install), version)
			}
			fmt.Fprintf(stdout, "%s %s: %s\n", c, name, entry)
		}
	}
	return nil
}
