use super::combinators::Op;

/// Parse a pipeline string into a Vec of Ops.
/// A Pipeline is `Stage ("|" Stage)*`.
/// A Stage is an Ident followed by optional key=value args.
pub fn parse(input: &str) -> Result<Vec<Op>, String> {
    let stages: Vec<&str> = input.split('|').collect();
    let mut ops = Vec::new();
    for stage in stages {
        let stage = stage.trim();
        if stage.is_empty() {
            return Err("empty pipeline stage".into());
        }
        ops.push(parse_stage(stage)?);
    }
    Ok(ops)
}

fn parse_stage(stage: &str) -> Result<Op, String> {
    // Handle nix { ... } specially — capture raw content inside braces
    if stage.starts_with("nix") {
        let rest = stage["nix".len()..].trim();
        if rest.starts_with('{') && rest.ends_with('}') {
            let content = rest[1..rest.len() - 1].trim().to_string();
            return Ok(Op::Nix(content));
        } else {
            return Err(format!("nix stage must have {{...}} body, got: {}", stage));
        }
    }

    // Tokenize the rest (space-separated, key=value pairs)
    let mut tokens = stage.split_whitespace();
    let name = tokens.next().ok_or_else(|| "empty stage".to_string())?;

    // Collect key=value pairs and bare flags
    let mut kwargs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut flags: Vec<String> = Vec::new();
    let mut positional: Option<String> = None;

    for token in tokens {
        if let Some((k, v)) = token.split_once('=') {
            kwargs.insert(k.to_string(), v.to_string());
        } else {
            // Could be a positional value (e.g., "top 3") or a bare flag (e.g., "desc")
            if positional.is_none() && token.parse::<usize>().is_ok() {
                positional = Some(token.to_string());
            } else {
                flags.push(token.to_string());
            }
        }
    }

    let get = |key: &str| -> Result<String, String> {
        kwargs
            .get(key)
            .cloned()
            .ok_or_else(|| format!("missing required arg '{}' in stage '{}'", key, name))
    };

    match name {
        "passthrough" => Ok(Op::Passthrough),
        "filter" => Ok(Op::Filter {
            field: get("field")?,
            value: get("value")?,
        }),
        "map" => Ok(Op::Map { field: get("field")? }),
        "sort" => Ok(Op::Sort {
            field: get("field")?,
            desc: flags.contains(&"desc".to_string()),
        }),
        "top" => {
            let n = positional
                .or_else(|| kwargs.get("n").cloned())
                .ok_or_else(|| format!("top requires a count, e.g. 'top 3'"))?;
            let n: usize = n.parse().map_err(|_| format!("top: invalid count '{}'", n))?;
            Ok(Op::Top(n))
        }
        "recurse" => Ok(Op::Recurse {
            op: Box::new(Op::Passthrough),
            max_depth: kwargs
                .get("max_depth")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
        }),
        "neighbors" => Ok(Op::Neighbors {
            depth: kwargs
                .get("depth")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
        }),
        "community" => Ok(Op::Community),
        "centrality" => Ok(Op::Centrality { metric: get("metric")? }),
        "zip" => Ok(Op::Zip { right: vec![] }),
        "self-join" => Ok(Op::SelfJoin),
        "stats" => Ok(Op::Stats {
            func: get("func")?,
            field: get("field")?,
        }),
        "dedup" => Ok(Op::Dedup { field: get("field")? }),
        other => Err(format!("unknown pipeline stage: '{}'", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_passthrough() {
        assert_eq!(parse("passthrough").unwrap(), vec![Op::Passthrough]);
    }

    #[test]
    fn test_parse_pipe() {
        let ops = parse("passthrough | top 3").unwrap();
        assert_eq!(ops, vec![Op::Passthrough, Op::Top(3)]);
    }

    #[test]
    fn test_parse_filter() {
        let ops = parse("filter field=id value=foo").unwrap();
        assert_eq!(
            ops,
            vec![Op::Filter {
                field: "id".into(),
                value: "foo".into()
            }]
        );
    }

    #[test]
    fn test_parse_sort_desc() {
        let ops = parse("sort field=name desc").unwrap();
        assert_eq!(
            ops,
            vec![Op::Sort {
                field: "name".into(),
                desc: true
            }]
        );
    }

    #[test]
    fn test_parse_nix() {
        let ops = parse("nix { 1 + 1 }").unwrap();
        assert_eq!(ops, vec![Op::Nix("1 + 1".into())]);
    }

    #[test]
    fn test_parse_unknown_error() {
        assert!(parse("frobniculate").is_err());
    }
}
