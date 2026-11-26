set shell := ["bash", "-euo", "pipefail", "-c"]

export CGO_ENABLED := "0"

alias b := build
alias c := clean
alias f := fmt
alias i := install
alias t := test
alias u := uninstall
alias v := validate

[default]
default:
    @just --list

build:
    go build -trimpath -ldflags='-s -w' .

clean:
    go clean

fmt:
    gofmt -w .

fmt-check:
    test -z "$(gofmt -l .)"

install:
    go install -trimpath -ldflags='-s -w' .

run *args:
    go run . {{ args }}

test:
    go test ./...

uninstall:
    go run . remove || true
    gobin="$(go env GOBIN)"; gopath="$(go env GOPATH)"; gopath="${gopath:?}"; gio trash --force -- "${gobin:-${gopath%%:*}/bin}/nv"
    go clean
    gio trash --force -- "${HOME:?HOME is not set}/.local/share/nv"

validate: fmt-check vet test

vet:
    go vet ./...
