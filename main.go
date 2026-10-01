package main

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"time"
)

const (
	assetName = "nvim-linux-x86_64.tar.gz"
	usage     = `Usage:
  nv install stable|nightly
  nv use stable|nightly
  nv update [stable|nightly]
  nv remove [stable|nightly]
  nv rollback stable|nightly
  nv status
  nv help`
)

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

var pointers = []string{"current", "previous"}

func parseChannel(value string) (channel, error) {
	if c := channel(value); slices.Contains(channels, c) {
		return c, nil
	}
	return "", fmt.Errorf("unsupported channel %q; expected stable or nightly", value)
}

func (c channel) apiURL() string {
	if c == stable {
		return apiBase + "/repos/neovim/neovim/releases/latest"
	}
	return apiBase + "/repos/neovim/neovim/releases/tags/nightly"
}

func (c channel) activeTarget() string {
	return filepath.Join("channels", string(c), "current")
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
	command, err := parseCommand(args)
	if err != nil {
		return err
	}
	p, err := newPaths()
	if err != nil {
		return err
	}
	return command(p)
}

func parseCommand(args []string) (func(paths) error, error) {
	if len(args) == 1 {
		switch args[0] {
		case "update":
			return func(p paths) error { return update(p, "") }, nil
		case "remove":
			return func(p paths) error { return remove(p, "") }, nil
		case "status":
			return status, nil
		}
	} else if len(args) == 2 {
		var command func(paths, channel) error
		switch args[0] {
		case "install":
			command = install
		case "use":
			command = use
		case "update":
			command = update
		case "remove":
			command = remove
		case "rollback":
			command = rollback
		}
		if command != nil {
			c, err := parseChannel(args[1])
			if err != nil {
				return nil, err
			}
			return func(p paths) error { return command(p, c) }, nil
		}
	}
	return nil, fmt.Errorf("invalid arguments\n\n%s", usage)
}

func install(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	if current == "" {
		return updateChannel(p, c)
	}
	version, err := nvimVersion(p.installDir(current))
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "%s is already installed: %s (release %s)\n", c, version, releaseID(current))
	return nil
}

func use(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	if current == "" {
		if err := updateChannel(p, c); err != nil {
			return err
		}
	}
	return activate(p, c)
}

func update(p paths, selection channel) error {
	selected, err := installed(p, selection)
	if err != nil {
		return err
	}
	for _, c := range selected {
		if err := updateChannel(p, c); err != nil {
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
	if slices.Contains(selected, activeChannel(p)) {
		if err := removeLink(p.active); err != nil {
			return err
		}
		if err := removeLink(p.nvimLink); err != nil {
			return err
		}
	}
	for _, c := range selected {
		for _, name := range pointers {
			if err := removeLink(p.channelLink(c, name)); err != nil {
				return err
			}
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
	version, err := nvimVersion(p.installDir(previous))
	if err != nil {
		return err
	}
	fmt.Fprintf(stdout, "rolled back %s to %s (release %s)\n", c, version, releaseID(previous))
	return nil
}

func status(p paths) error {
	active := "none"
	if c := activeChannel(p); c != "" {
		active = string(c)
	}
	fmt.Fprintf(stdout, "active: %s\n", active)
	for _, c := range channels {
		for _, name := range pointers {
			install, err := p.readPointer(c, name)
			if err != nil {
				return err
			}
			entry := "none"
			if install != "" {
				version, err := nvimVersion(p.installDir(install))
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

func updateChannel(p paths, c channel) error {
	current, err := p.readPointer(c, "current")
	if err != nil {
		return err
	}
	r, err := resolveRelease(c)
	if err != nil {
		return err
	}
	name := fmt.Sprintf("%s-%d", c, r.id)
	target := p.installDir(name)
	if current == name {
		version, err := nvimVersion(target)
		if err != nil {
			return err
		}
		fmt.Fprintf(stdout, "%s is already current: %s (release %d)\n", c, version, r.id)
		return nil
	}
	var version string
	if _, err = os.Stat(target); err == nil {
		version, err = nvimVersion(target)
	} else {
		version, err = fetch(r, p.staging, target)
	}
	if err != nil {
		return err
	}
	if err := p.writePointer(c, "current", name); err != nil {
		return err
	}
	if current != "" {
		if err := p.writePointer(c, "previous", current); err != nil {
			return err
		}
	}
	if err := cleanup(p); err != nil {
		return err
	}
	fmt.Fprintf(stdout, "installed %s %s (release %d)\n", c, version, r.id)
	return nil
}
