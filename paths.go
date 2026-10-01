package main

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
)

type paths struct {
	installs    string
	channelsDir string
	staging     string
	active      string
	nvimLink    string
}

func newPaths() (paths, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return paths{}, err
	}
	state := filepath.Join(home, ".local/share/nv")
	p := paths{
		installs:    filepath.Join(state, "installs"),
		channelsDir: filepath.Join(state, "channels"),
		staging:     filepath.Join(state, "staging"),
		active:      filepath.Join(state, "active"),
		nvimLink:    filepath.Join(home, ".local/bin/nvim"),
	}
	dirs := []string{p.installs, filepath.Dir(p.nvimLink)}
	for _, c := range channels {
		dirs = append(dirs, filepath.Join(p.channelsDir, string(c)))
	}
	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return paths{}, err
		}
	}
	return p, nil
}

func (p paths) installDir(name string) string {
	return filepath.Join(p.installs, name)
}

func (p paths) channelLink(c channel, name string) string {
	return filepath.Join(p.channelsDir, string(c), name)
}

func (p paths) nvimTarget() string {
	return filepath.Join(p.active, "bin/nvim")
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

func activate(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	target, err := os.Readlink(p.nvimLink)
	managed := errors.Is(err, fs.ErrNotExist) || err == nil && target == p.nvimTarget()
	if !managed {
		return fmt.Errorf("%s is not managed by nv; refusing to replace it", p.nvimLink)
	}
	if err := replaceLink(p.active, c.activeTarget()); err != nil {
		return err
	}
	if err := replaceLink(p.nvimLink, p.nvimTarget()); err != nil {
		return err
	}
	version, err := nvimVersion(p.installDir(current))
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "using %s %s (release %s)\n", c, version, releaseID(current))
	return nil
}

func activeChannel(p paths) channel {
	target, err := os.Readlink(p.active)
	if err != nil {
		return ""
	}
	for _, c := range channels {
		if target == c.activeTarget() {
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
	if len(result) == 0 {
		if selection != "" {
			return nil, fmt.Errorf("%s is not installed", selection)
		}
		return nil, errors.New("no channels are installed")
	}
	return result, nil
}

func cleanup(p paths) error {
	var referenced []string
	for _, c := range channels {
		for _, name := range pointers {
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
			if err := os.RemoveAll(p.installDir(entry.Name())); err != nil {
				return err
			}
		}
	}
	return nil
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
