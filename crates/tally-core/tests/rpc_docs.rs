use tally_core::wire::RPC_METHODS;

const RPC_DOC: &str = include_str!("../../../doc/src/reference/rpc-protocol.md");

fn documented_methods() -> Vec<&'static str> {
    let (_, after_start) = RPC_DOC
        .split_once("<!-- rpc-method-list:start -->")
        .expect("RPC reference must contain the method-list start marker");
    let (table, _) = after_start
        .split_once("<!-- rpc-method-list:end -->")
        .expect("RPC reference must contain the method-list end marker");

    table
        .lines()
        .filter_map(|line| {
            let first_cell = line.split('|').nth(1)?.trim();
            first_cell
                .strip_prefix('`')
                .and_then(|cell| cell.strip_suffix('`'))
                .filter(|cell| cell.contains('.'))
        })
        .collect()
}

#[test]
fn advertised_rpc_methods_match_the_reference_table() {
    assert_eq!(documented_methods(), RPC_METHODS);
}
