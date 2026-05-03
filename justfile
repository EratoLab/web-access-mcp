default:
	just --list

test:
	cargo test --all-features

run_chrome *args:
	cargo run --features mcp-server --bin mcp-server -- --browser chrome --transport http --port 51802 {{args}}

run_lightpanda *args:
	cargo run --features mcp-server --bin mcp-server -- --browser lightpanda --transport http --port 51802 {{args}}
