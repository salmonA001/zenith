//! Turns a command line into a command's JSON arguments, by what the table says it takes.
use serde_json::{Map, Value, json};
use zenith_agent::{ArgKind, ArgSpec, CommandSpec};

fn convert(a: &ArgSpec, text: &str) -> Result<Value, String> {
    match a.kind {
        ArgKind::Int => {
            text.parse::<i64>().map(|v| json!(v)).map_err(|_| format!("{} must be a whole number, not {text}", a.name))
        }
        ArgKind::Number => {
            text.parse::<f64>().map(|v| json!(v)).map_err(|_| format!("{} must be a number, not {text}", a.name))
        }
        ArgKind::Text => Ok(json!(text)),
        ArgKind::Flag => Ok(json!(true)),
    }
}

/// Reads arguments by position, in the table's order, or as `--name value`, or `--flag`.
pub fn parse(spec: &CommandSpec, words: &[String]) -> Result<Value, String> {
    let mut out = Map::new();
    let mut positional = spec.args.iter().filter(|a| a.kind != ArgKind::Flag);
    let mut it = words.iter();
    while let Some(word) = it.next() {
        if let Some(name) = word.strip_prefix("--") {
            let name = name.replace('-', "_");
            let a = spec
                .args
                .iter()
                .find(|a| a.name == name)
                .ok_or_else(|| format!("{} has no option --{name}", spec.name))?;
            if a.kind == ArgKind::Flag {
                out.insert(a.name.into(), json!(true));
            } else {
                let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
                out.insert(a.name.into(), convert(a, value)?);
            }
            continue;
        }
        let a = loop {
            match positional.next() {
                Some(a) if out.contains_key(a.name) => {}
                Some(a) => break a,
                None => return Err(format!("{} takes no more arguments: {word}", spec.name)),
            }
        };
        out.insert(a.name.into(), convert(a, word)?);
    }
    // Text goes last and takes the rest of the line, so `chat hello there` means what it looks like.
    for a in spec.args {
        if a.required && !out.contains_key(a.name) {
            return Err(format!("{} needs {}", spec.name, a.name));
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_agent::commands::find;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn arguments_go_by_position_with_the_right_types() {
        let v = parse(find("block").expect("block"), &words("1 -2 3")).expect("parses");
        assert_eq!(v, json!({"x": 1, "y": -2, "z": 3}));
    }

    #[test]
    fn options_can_be_named_and_flags_stand_alone() {
        let v = parse(find("goto").expect("goto"), &words("1.5 2 3 --near 0.5 --sprint")).expect("parses");
        assert_eq!(v, json!({"x": 1.5, "y": 2.0, "z": 3.0, "near": 0.5, "sprint": true}));
    }

    #[test]
    fn dashes_and_underscores_both_work_for_names() {
        let v = parse(find("find-block").expect("find_block"), &words("oak_log --radius 8")).expect("parses");
        assert_eq!(v, json!({"block": "oak_log", "radius": 8}));
    }

    #[test]
    fn mistakes_are_explained() {
        let goto = find("goto").expect("goto");
        assert!(parse(goto, &words("1 2")).expect_err("short").contains("needs z"));
        assert!(parse(goto, &words("a 2 3")).expect_err("not a number").contains("must be a number"));
        assert!(parse(goto, &words("1 2 3 --nope")).expect_err("unknown").contains("no option"));
        assert!(parse(goto, &words("1 2 3 --near")).expect_err("no value").contains("needs a value"));
        assert!(parse(find("state").expect("state"), &words("extra")).expect_err("extra").contains("no more"));
    }
}
