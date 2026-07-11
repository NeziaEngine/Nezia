// nezia-cli — NEZIA ENGINE daemon の front door。
// 設計は docs/design/cli/CONCEPT.md を正とする。
package main

import "jp.nezia/nezia-cli/internal/command"

func main() {
	command.Main()
}
