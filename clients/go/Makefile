STATICCHECK_VERSION := v0.8.2
REPO_ROOT := ../..

.PHONY: check fmt-check vet staticcheck test build scenarios

check: fmt-check vet staticcheck test build

fmt-check:
	@unformatted="$$(gofmt -l .)"; if [ -n "$$unformatted" ]; then echo "gofmt needed:"; echo "$$unformatted"; exit 1; fi

vet:
	go vet ./...

staticcheck:
	go run honnef.co/go/tools/cmd/staticcheck@$(STATICCHECK_VERSION) ./...

test:
	go test -race -count=1 ./...

build:
	go build -o bin/popsub-driver ./cmd/popsub-driver

scenarios: build
	cd $(REPO_ROOT) && target/debug/popsub-scenarios --driver "clients/go/bin/popsub-driver"
