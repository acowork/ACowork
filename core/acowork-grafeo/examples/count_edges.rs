//! One-off diagnostic: count nodes/edges in a Grafeo store snapshot.
//! Usage: cargo run -p acowork-grafeo --example count_edges -- <path-to.grafeo>

fn main() {
    let path = std::env::args().nth(1).expect("usage: count_edges <path>");
    let store = acowork_grafeo::GrafeoStore::open_with_default_config(std::path::Path::new(&path))
        .expect("open store");
    let db = store.db();
    println!("nodes: {}", db.node_count());
    println!("edges: {}", db.edge_count());

    // Edge breakdown by type via GQL.
    let session = db.session();
    if let Ok(result) = session.execute("MATCH ()-[r]->() RETURN type(r), count(r)") {
        for row in result.rows() {
            println!("edge row: {row:?}");
        }
    } else {
        println!("(GQL edge breakdown unavailable)");
    }
}
