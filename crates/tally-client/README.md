# tally-client

`tally-client` is the reference Rust implementation of tally's versioned Unix-socket
NDJSON-RPC protocol. It provides the shared request and error types, the multiplexed RPC
client, and resolution of the symmetric frame limit from tally's rendered configuration.

The crate has no dependency on `tally-core` or any daemon implementation. In-tree clients
and future Rust surfaces can therefore speak the wire protocol without linking the tally
kernel.
