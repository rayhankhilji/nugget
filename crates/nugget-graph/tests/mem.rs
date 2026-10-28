#[test]
fn mem_graph_conforms() {
    nugget_graph::conformance::run(&|| Box::new(nugget_graph::MemGraph::new()));
}
