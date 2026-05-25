use regex_lite::Regex;
use std::sync::OnceLock;

fn fenced_block_rx() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?ms)^```(bash|sh|zsh|python|py|js|javascript|node|ruby|perl|php)\b").unwrap()
    })
}

fn curl_pipe_rx() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"curl\s+[^|]+\|\s*(sudo\s+)?(sh|bash|zsh|python|py|node|ruby|perl)").unwrap()
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExecutableFinding {
    pub kind: ExecutableKind,
    pub matched: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ExecutableKind {
    FencedCodeBlock,
    CurlPipeShell,
}

pub fn scan_executable(body: &str) -> Vec<ExecutableFinding> {
    let mut out = Vec::new();
    for m in fenced_block_rx().find_iter(body) {
        out.push(ExecutableFinding {
            kind: ExecutableKind::FencedCodeBlock,
            matched: m.as_str().to_string(),
        });
    }
    for m in curl_pipe_rx().find_iter(body) {
        out.push(ExecutableFinding {
            kind: ExecutableKind::CurlPipeShell,
            matched: m.as_str().to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_fence_flagged() {
        let body = "Run this:\n```bash\nrm -rf /\n```\n";
        let f = scan_executable(body);
        assert!(f.iter().any(|x| x.kind == ExecutableKind::FencedCodeBlock));
    }

    #[test]
    fn python_fence_flagged() {
        let body = "```python\nimport os\n```\n";
        let f = scan_executable(body);
        assert!(f.iter().any(|x| x.kind == ExecutableKind::FencedCodeBlock));
    }

    #[test]
    fn yaml_fence_allowed() {
        let body = "```yaml\nname: x\n```\n";
        assert!(scan_executable(body).is_empty());
    }

    #[test]
    fn curl_pipe_sh_flagged() {
        let body = "Install: curl https://x.com/install.sh | sh";
        let f = scan_executable(body);
        assert!(f.iter().any(|x| x.kind == ExecutableKind::CurlPipeShell));
    }

    #[test]
    fn plain_prose_clean() {
        assert!(scan_executable("just regular markdown text").is_empty());
    }
}
