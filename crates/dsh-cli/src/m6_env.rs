//! M6 step7（D-086）：`.env` 解析 + 键注入 server 装配。
//!
//! 诚实原则（IV-3）：`.env` 只是**进程环境的上游可选来源**——解析出的 KEY=VALUE 仅
//! apply 进进程环境（既有环境变量优先，>overwrite:false），服务器装配的 env 读取链
//! （`DSH_LLM_BASE_URL` / `DSH_LLM_MODEL` / `DEEPSEEK_API_KEY` …）透明吃到 overlay。
//! key 永不落 settings / 库 / git（P4）。解析失败 → `Err`（fail-loud，行号定位），
//! 绝不静默跳过坏行。无插值、无导出语义（保持最小面；文档化为近似 dotenv 子集）。

use std::path::Path;

/// 解析 `.env` 文本 → KEY/VALUE 对。规则：空白行与 `#` 注释跳过；`KEY=VALUE`（键值
/// 两侧允许空白）；值可选单/双引号包裹；容忍 CRLF；**不**做内联注释/插值（文档化为
/// dotenv 子集）。非注释非空行缺 `=` 或有空键 → `Err`（fail-loud，行号 + 现场）。
pub fn parse_env_file(input: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else {
            return Err(format!(
                "env file line {line_no}: missing '=' (expected KEY=VALUE), offending: {line:?}"
            ));
        };
        let key = line[..eq].trim().to_string();
        if key.is_empty() {
            return Err(format!("env file line {line_no}: empty key before '='"));
        }
        let mut value = line[eq + 1..].trim().to_string();
        // 可选成对引号剥除（单/双；剥后含空格是合法的）。
        let vlen = value.len();
        if vlen >= 2 {
            let first = value.chars().next().unwrap();
            let last = value.chars().last().unwrap();
            if (first == '"' || first == '\'') && first == last {
                value = value[1..vlen - 1].to_string();
            }
        }
        out.push((key, value));
    }
    Ok(out)
}

/// 从磁盘读 `.env` 文件并解析（io 错 → `Err` 含路径；解析错含行号）。
pub fn load_env_file(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("env file {}: {e}", path.display()))?;
    parse_env_file(&text)
}

/// 把解析出的键 apply 进进程环境（**overwrite:false**——既有环境变量优先）。返回值 =
/// 实际写入条数。侧效应用，不落 settings/库/git（IV-3）。
pub fn apply_env_into_process(pairs: &[(String, String)]) -> usize {
    let mut applied = 0;
    for (k, v) in pairs {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
            applied += 1;
        }
    }
    applied
}

/// serve 装配入口：`None` → no-op；`Some(path)` → load + apply（fail-loud 解析/读错）。
pub fn apply_env_file(path: Option<&Path>) -> Result<usize, String> {
    match path {
        None => Ok(0),
        Some(p) => {
            let pairs = load_env_file(p)?;
            Ok(apply_env_into_process(&pairs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_env_file_basic_and_ignores_comments_blanks() {
        let pairs = parse_env_file(
            "# a comment\n\nKEY=value\nDSH_LLM_BASE_URL=http://x:1\n  DSH_LLM_MODEL  =  m1  \n",
        )
        .expect("parse ok");
        assert_eq!(pairs[0], ("KEY".to_string(), "value".to_string()));
        assert_eq!(
            pairs[1],
            ("DSH_LLM_BASE_URL".to_string(), "http://x:1".to_string())
        );
        assert_eq!(pairs[2], ("DSH_LLM_MODEL".to_string(), "m1".to_string()));
    }

    #[test]
    fn parse_env_file_quoted_and_crlf() {
        let pairs = parse_env_file("A=\"quoted value\"\r\nB='single'\r\nEMPTY=\r\n")
            .expect("parse ok");
        assert_eq!(pairs[0], ("A".to_string(), "quoted value".to_string()));
        assert_eq!(pairs[1], ("B".to_string(), "single".to_string()));
        assert_eq!(pairs[2], ("EMPTY".to_string(), String::new()));
    }

    #[test]
    fn parse_env_file_missing_equals_fails_loud_with_line() {
        let err = parse_env_file("GOOD=1\nTHIS_IS_BROKEN\nAFTER=2").expect_err("must fail");
        assert!(err.contains("2"), "line number reported: {err}");
        assert!(err.contains("THIS_IS_BROKEN"), "offending line named: {err}");
    }

    #[test]
    fn load_env_file_reads_disk_and_apply_overlay_respects_existing_env() {
        let path = std::env::temp_dir().join(format!("dsh-m6-env-{}.env", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "DSH_M6_TEST_A=from-file").unwrap();
        writeln!(f, "DSH_M6_TEST_OVERRIDE=from-file").unwrap();
        writeln!(f, "DSH_M6_IDEMPOTENT=X").unwrap();
        drop(f);
        let pairs = load_env_file(&path).expect("load ok");
        assert_eq!(pairs.len(), 3);

        // 既有环境变量优先（overwrite:false）——用独特键避免并行污染其它测试。
        std::env::set_var("DSH_M6_TEST_OVERRIDE", "from-process");
        let applied = apply_env_into_process(&pairs);
        assert_eq!(applied, 2, "existing-env key not overwritten");
        assert_eq!(
            std::env::var("DSH_M6_TEST_OVERRIDE").unwrap(),
            "from-process",
            "pre-existing env wins"
        );
        assert_eq!(std::env::var("DSH_M6_TEST_A").unwrap(), "from-file");

        let _ = std::fs::remove_file(&path);
        std::env::remove_var("DSH_M6_TEST_A");
        std::env::remove_var("DSH_M6_TEST_OVERRIDE");
        std::env::remove_var("DSH_M6_IDEMPOTENT");
    }
}
