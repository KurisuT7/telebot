#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub prefix: String,
    pub name: String,
    pub raw_args: String,
    pub args: Vec<String>,
}

pub fn parse(text: &str, prefixes: &[String]) -> Option<Command> {
    let trimmed = text.trim_start();
    let prefix = prefixes
        .iter()
        .filter(|prefix| trimmed.starts_with(prefix.as_str()))
        .max_by_key(|prefix| prefix.chars().count())?;
    let body = trimmed[prefix.len()..].trim_start();
    if body.is_empty() {
        return None;
    }

    let command_end = body.find(char::is_whitespace).unwrap_or(body.len());
    let name = body[..command_end].to_ascii_lowercase();
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    let raw_args = body[command_end..].trim().to_owned();
    let args = raw_args.split_whitespace().map(str::to_owned).collect();
    Some(Command {
        prefix: prefix.clone(),
        name,
        raw_args,
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefixes() -> Vec<String> {
        [".", "。", ",", "，"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn parses_all_requested_prefixes() {
        for prefix in [".", "。", ",", "，"] {
            let parsed = parse(&format!("{prefix}AI search rust"), &prefixes()).unwrap();
            assert_eq!(parsed.name, "ai");
            assert_eq!(parsed.args, ["search", "rust"]);
        }
    }

    #[test]
    fn ignores_normal_punctuation() {
        assert!(parse("。 这不是命令", &prefixes()).is_none());
        assert!(parse("hello.ai", &prefixes()).is_none());
    }
}
