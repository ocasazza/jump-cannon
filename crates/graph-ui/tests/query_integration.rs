use graph_ui::query::{evaluate, parse};
use graph_ui::query::eval::MockGraph;

fn five_nodes() -> MockGraph {
    MockGraph {
        nodes: vec![
            "alpha".into(),
            "beta".into(),
            "gamma".into(),
            "delta".into(),
            "epsilon".into(),
        ],
    }
}

fn ten_nodes() -> MockGraph {
    MockGraph {
        nodes: (0..10).map(|i| format!("node-{:02}", i)).collect(),
    }
}

mod integration {
    use super::*;

    #[test]
    fn pipe_passthrough_top() {
        let ops = parse("passthrough | top 3").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn filter_by_id() {
        let ops = parse("filter field=id value=alpha").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert_eq!(result, vec!["alpha"]);
    }

    #[test]
    fn sort_asc() {
        let ops = parse("sort field=id").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert_eq!(result[0], "alpha");
        assert_eq!(result[4], "gamma"); // alphabetical last among the 5
    }

    #[test]
    fn sort_desc() {
        let ops = parse("sort field=id desc").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert_eq!(result[0], "gamma");
    }

    #[test]
    fn top_limits() {
        let ops = parse("top 2").unwrap();
        assert_eq!(evaluate(&ops, &ten_nodes()).unwrap().len(), 2);
    }

    #[test]
    fn top_larger_than_set() {
        let ops = parse("top 100").unwrap();
        assert_eq!(evaluate(&ops, &five_nodes()).unwrap().len(), 5);
    }

    #[test]
    fn neighbors_expands() {
        let ops = parse("neighbors depth=1").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert!(result.len() > 5, "neighbors should expand the set");
    }

    #[test]
    fn nix_escape_hatch() {
        let ops = parse("nix { 1 + 1 }").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert_eq!(result, vec!["2"]);
    }

    #[test]
    fn chained_pipe() {
        // filter then top
        let ops = parse("passthrough | sort field=id | top 1").unwrap();
        let result = evaluate(&ops, &five_nodes()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "alpha"); // first after sort asc
    }

    #[test]
    fn unknown_op_errors() {
        assert!(parse("definitely_not_an_op").is_err());
    }
}
