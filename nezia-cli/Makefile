# nezia-cli — build / codegen / test
#
# 生成コードは Go 慣習に従い commit する (docs/design/cli/CONCEPT.md §2)。
# protoc 本体は brew 等で導入済みであること。plugin はローカル bin/ に固定する。

PROTO_DIR   := ../proto
PROTO_FILES := $(PROTO_DIR)/nezia/v1/common.proto $(PROTO_DIR)/nezia/v1/daemon.proto
GEN_PKG     := jp.nezia/nezia-cli/internal/gen/neziav1
GO_OPT      := Mnezia/v1/common.proto=$(GEN_PKG),Mnezia/v1/daemon.proto=$(GEN_PKG)
VERSION     := $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
LDFLAGS     := -X jp.nezia/nezia-cli/internal/command.version=$(VERSION)

.PHONY: build gen test vet tools clean

build:
	go build -ldflags "$(LDFLAGS)" -o bin/nezia-cli ./cmd/nezia-cli

gen: tools
	PATH=$(PWD)/bin:$$PATH protoc -I $(PROTO_DIR) \
		--go_out=. --go_opt=module=jp.nezia/nezia-cli --go_opt=$(GO_OPT) \
		--go-grpc_out=. --go-grpc_opt=module=jp.nezia/nezia-cli --go-grpc_opt=$(GO_OPT) \
		$(PROTO_FILES)

tools:
	@test -x bin/protoc-gen-go || GOBIN=$(PWD)/bin go install google.golang.org/protobuf/cmd/protoc-gen-go@v1.36.6
	@test -x bin/protoc-gen-go-grpc || GOBIN=$(PWD)/bin go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@v1.5.1

test:
	go test ./...

vet:
	go vet ./...

clean:
	rm -rf bin/nezia-cli
