package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

type fakeGitHub struct {
	url      string
	release  string
	digest   string
	requests int
}

func (f *fakeGitHub) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	f.requests++
	archive := f.archive()
	switch r.URL.Path {
	case "/repos/neovim/neovim/releases/latest", "/repos/neovim/neovim/releases/tags/nightly":
		digest := f.digest
		if digest == "" {
			digest = fmt.Sprintf("%x", sha256.Sum256(archive))
		}
		fmt.Fprintf(w, `{"id":%s,"assets":[{"name":%q,"browser_download_url":"%s/download","digest":"sha256:%s"}]}`,
			f.release, assetName, f.url, digest)
	case "/download":
		w.Write(archive)
	default:
		http.NotFound(w, r)
	}
}

func (f *fakeGitHub) archive() []byte {
	var buffer bytes.Buffer
	zipped := gzip.NewWriter(&buffer)
	writer := tar.NewWriter(zipped)
	for _, dir := range []string{"nvim-linux-x86_64/", "nvim-linux-x86_64/bin/"} {
		writer.WriteHeader(&tar.Header{Name: dir, Typeflag: tar.TypeDir, Mode: 0o755})
	}
	script := "#!/bin/sh\necho " + f.release + "\n"
	writer.WriteHeader(&tar.Header{Name: "nvim-linux-x86_64/bin/nvim", Mode: 0o755, Size: int64(len(script))})
	writer.Write([]byte(script))
	writer.Close()
	zipped.Close()
	return buffer.Bytes()
}

func setup(t *testing.T) (*fakeGitHub, string) {
	github := &fakeGitHub{}
	server := httptest.NewTLSServer(github)
	t.Cleanup(server.Close)
	github.url = server.URL
	apiBase, client = server.URL, server.Client()
	home := t.TempDir()
	t.Setenv("HOME", home)
	return github, home
}

func nv(args ...string) (string, error) {
	var output strings.Builder
	stdout = &output
	err := run(args)
	return output.String(), err
}

func must(t *testing.T, args ...string) {
	t.Helper()
	if _, err := nv(args...); err != nil {
		t.Fatalf("nv %s: %s", strings.Join(args, " "), err)
	}
}

func equal[T comparable](t *testing.T, got, want T) {
	t.Helper()
	if got != want {
		t.Fatalf("got %v, want %v", got, want)
	}
}

func fails(t *testing.T, message string, args ...string) {
	t.Helper()
	if _, err := nv(args...); err == nil || !strings.Contains(err.Error(), message) {
		t.Fatalf("nv %s: unexpected error: %v", strings.Join(args, " "), err)
	}
}

func TestLifecycle(t *testing.T) {
	github, home := setup(t)
	nvim := filepath.Join(home, ".local/bin/nvim")
	version := func() string {
		t.Helper()
		output, err := exec.Command(nvim).Output()
		if err != nil {
			t.Fatal(err)
		}
		return string(output)
	}

	github.release = "100"
	must(t, "use", "stable")
	equal(t, version(), "100\n")
	github.release = "101"
	must(t, "update")
	equal(t, version(), "101\n")
	github.release = "102"
	requests := github.requests
	must(t, "use", "stable")
	equal(t, version(), "101\n")
	output, err := nv("install", "stable")
	if err != nil {
		t.Fatal(err)
	}
	equal(t, output, "stable is already installed: 101 (release 101)\n")
	equal(t, github.requests, requests)
	must(t, "install", "nightly")
	equal(t, version(), "101\n")
	output, err = nv("status")
	if err != nil {
		t.Fatal(err)
	}
	equal(t, output, "active: stable\n"+
		"stable current: release=101 version=101\n"+
		"stable previous: release=100 version=100\n"+
		"nightly current: release=102 version=102\n"+
		"nightly previous: none\n")
	fails(t, "no previous installation", "rollback", "nightly")
	must(t, "rollback", "stable")
	equal(t, version(), "100\n")
	github.release = "101"
	requests = github.requests
	must(t, "update", "stable")
	equal(t, version(), "101\n")
	equal(t, github.requests, requests+1)
	must(t, "remove")
	if _, err := os.Lstat(nvim); err == nil {
		t.Fatalf("%s still exists", nvim)
	}
	fails(t, "no channels are installed", "remove")
	fails(t, "nightly is not installed", "update", "nightly")
	fails(t, "nightly is not installed", "remove", "nightly")
}

func TestChecksumMismatch(t *testing.T) {
	github, _ := setup(t)
	github.release, github.digest = "100", "bad"
	fails(t, "SHA-256 mismatch", "install", "stable")
	github.digest = ""
	output, err := nv("status")
	if err != nil {
		t.Fatal(err)
	}
	equal(t, output, "active: none\n"+
		"stable current: none\n"+
		"stable previous: none\n"+
		"nightly current: none\n"+
		"nightly previous: none\n")
}

func TestUnmanagedLink(t *testing.T) {
	github, home := setup(t)
	github.release = "100"
	nvim := filepath.Join(home, ".local/bin/nvim")
	os.MkdirAll(filepath.Dir(nvim), 0o755)
	os.WriteFile(nvim, nil, 0o644)
	fails(t, "not managed by nv", "use", "stable")
	if info, err := os.Lstat(nvim); err != nil || !info.Mode().IsRegular() {
		t.Fatalf("%s was replaced", nvim)
	}
}

func TestSymlinkedBin(t *testing.T) {
	github, home := setup(t)
	github.release = "100"
	bin := filepath.Join(home, "bin")
	os.MkdirAll(bin, 0o755)
	os.MkdirAll(filepath.Join(home, ".local"), 0o755)
	os.Symlink(bin, filepath.Join(home, ".local/bin"))
	must(t, "use", "stable")
	output, err := exec.Command(filepath.Join(home, ".local/bin/nvim")).Output()
	if err != nil {
		t.Fatal(err)
	}
	equal(t, string(output), "100\n")
}
