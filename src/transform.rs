use anyhow::{Context, bail};
use glob::Pattern;
use rama::http::{
    HeaderMap, HeaderValue,
    header::{self, HeaderName},
};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct TransformConfig {
    pub transforms: Vec<TransformRule>,
}

#[derive(Debug, Clone)]
pub struct TransformRule {
    matcher: TransformMatcher,
    action: TransformAction,
    target: TransformTarget,
    pub stop: bool,
}

#[derive(Debug, Clone)]
enum TransformMatcher {
    UrlGlob(Pattern),
    ContentTypeGlob(Pattern),
    Everything,
}

#[derive(Debug, Clone)]
enum TransformAction {
    Replace { from: String, to: String },
}

#[derive(Debug, Clone)]
enum TransformTarget {
    Everything,
    Body,
    AllHeaders,
    Header(HeaderName),
    Cookies,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrebufferDisposition {
    Stream,
    BufferFrom(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrebufferOutcome {
    pub disposition: PrebufferDisposition,
    pub headers_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub headers_changed: bool,
    pub body_changed: bool,
}

impl TransformConfig {
    pub fn load_json(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("read transform config {}", path.display()))?;
        let raw: TransformFile = serde_json::from_str(&contents)
            .with_context(|| format!("parse transform config {}", path.display()))?;

        let transforms = raw
            .transforms
            .into_iter()
            .map(TransformRule::try_from)
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self { transforms })
    }

    pub fn apply_until_body(
        &self,
        request_url: &str,
        headers: &mut HeaderMap,
    ) -> anyhow::Result<PrebufferOutcome> {
        let mut headers_changed = false;

        for (index, rule) in self.transforms.iter().enumerate() {
            if !rule.matches(request_url, headers) {
                continue;
            }

            if rule.requires_body() {
                return Ok(PrebufferOutcome {
                    disposition: PrebufferDisposition::BufferFrom(index),
                    headers_changed,
                });
            }

            let triggered = rule.apply_headers(headers)?;
            headers_changed |= triggered;
            if triggered && rule.stop {
                return Ok(PrebufferOutcome {
                    disposition: PrebufferDisposition::Stream,
                    headers_changed,
                });
            }
        }

        Ok(PrebufferOutcome {
            disposition: PrebufferDisposition::Stream,
            headers_changed,
        })
    }

    pub fn apply_with_body(
        &self,
        start_index: usize,
        request_url: &str,
        headers: &mut HeaderMap,
        body: &mut Vec<u8>,
    ) -> anyhow::Result<ApplyOutcome> {
        let mut headers_changed = false;
        let mut body_changed = false;

        for rule in self.transforms.iter().skip(start_index) {
            if !rule.matches(request_url, headers) {
                continue;
            }

            let triggered = rule.apply(headers, body)?;
            headers_changed |= triggered.headers_changed;
            body_changed |= triggered.body_changed;
            if triggered.triggered && rule.stop {
                break;
            }
        }

        Ok(ApplyOutcome {
            headers_changed,
            body_changed,
        })
    }
}

impl TransformRule {
    fn matches(&self, request_url: &str, headers: &HeaderMap) -> bool {
        match &self.matcher {
            TransformMatcher::UrlGlob(pattern) => pattern.matches(request_url),
            TransformMatcher::ContentTypeGlob(pattern) => {
                normalized_content_type(headers).is_some_and(|value| pattern.matches(&value))
            }
            TransformMatcher::Everything => true,
        }
    }

    fn requires_body(&self) -> bool {
        matches!(
            self.target,
            TransformTarget::Body | TransformTarget::Everything
        )
    }

    fn apply_headers(&self, headers: &mut HeaderMap) -> anyhow::Result<bool> {
        match self.target {
            TransformTarget::Everything => self.apply_selected_headers(headers, |_| true),
            TransformTarget::Body => Ok(false),
            TransformTarget::AllHeaders => self.apply_selected_headers(headers, |_| true),
            TransformTarget::Header(ref name) => {
                self.apply_selected_headers(headers, |current| current == name)
            }
            TransformTarget::Cookies => {
                self.apply_selected_headers(headers, |current| current == header::SET_COOKIE)
            }
        }
    }

    fn apply(
        &self,
        headers: &mut HeaderMap,
        body: &mut Vec<u8>,
    ) -> anyhow::Result<RuleApplyOutcome> {
        let headers_changed = self.apply_headers(headers)?;
        let body_changed = match self.target {
            TransformTarget::Everything | TransformTarget::Body => self.apply_body(body),
            TransformTarget::AllHeaders | TransformTarget::Header(_) | TransformTarget::Cookies => {
                false
            }
        };

        Ok(RuleApplyOutcome {
            triggered: headers_changed || body_changed,
            headers_changed,
            body_changed,
        })
    }

    fn apply_selected_headers<F>(
        &self,
        headers: &mut HeaderMap,
        selector: F,
    ) -> anyhow::Result<bool>
    where
        F: Fn(&HeaderName) -> bool,
    {
        let mut entries = Vec::new();
        let mut changed = false;

        for (name, value) in headers.iter() {
            if selector(name) {
                let current = value
                    .to_str()
                    .with_context(|| format!("response header {name} is not valid text"))?;
                let updated = self.replace_text(current);
                changed |= updated != current;
                entries.push((
                    name.clone(),
                    HeaderValue::from_str(&updated)
                        .with_context(|| format!("rewritten response header {name} is invalid"))?,
                ));
            } else {
                entries.push((name.clone(), value.clone()));
            }
        }

        if changed {
            headers.clear();
            for (name, value) in entries {
                headers.append(name, value);
            }
        }

        Ok(changed)
    }

    fn apply_body(&self, body: &mut Vec<u8>) -> bool {
        let text = String::from_utf8_lossy(body);
        let updated = self.replace_text(&text);
        if updated == text {
            return false;
        }
        *body = updated.into_bytes();
        true
    }

    fn replace_text(&self, input: &str) -> String {
        match &self.action {
            TransformAction::Replace { from, to } => input.replace(from, to),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleApplyOutcome {
    triggered: bool,
    headers_changed: bool,
    body_changed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformFile {
    #[serde(default)]
    transforms: Vec<TransformRuleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformRuleFile {
    matcher: TransformMatcherFile,
    action: TransformActionFile,
    target: TransformTargetFile,
    #[serde(default)]
    stop: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TransformMatcherFile {
    UrlGlob { pattern: String },
    ContentTypeGlob { pattern: String },
    Everything {},
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TransformActionFile {
    Replace { from: String, to: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TransformTargetFile {
    Everything {},
    Body {},
    AllHeaders {},
    Header { name: String },
    Cookies {},
}

impl TryFrom<TransformRuleFile> for TransformRule {
    type Error = anyhow::Error;

    fn try_from(value: TransformRuleFile) -> Result<Self, Self::Error> {
        let matcher = match value.matcher {
            TransformMatcherFile::UrlGlob { pattern } => TransformMatcher::UrlGlob(
                Pattern::new(&pattern).with_context(|| format!("invalid url_glob {pattern}"))?,
            ),
            TransformMatcherFile::ContentTypeGlob { pattern } => TransformMatcher::ContentTypeGlob(
                Pattern::new(&pattern.to_ascii_lowercase())
                    .with_context(|| format!("invalid content_type_glob {pattern}"))?,
            ),
            TransformMatcherFile::Everything {} => TransformMatcher::Everything,
        };

        let action = match value.action {
            TransformActionFile::Replace { from, to } => {
                if from.is_empty() {
                    bail!("replace transform requires a non-empty from value");
                }
                TransformAction::Replace { from, to }
            }
        };

        let target = match value.target {
            TransformTargetFile::Everything {} => TransformTarget::Everything,
            TransformTargetFile::Body {} => TransformTarget::Body,
            TransformTargetFile::AllHeaders {} => TransformTarget::AllHeaders,
            TransformTargetFile::Header { name } => {
                let normalized = name.trim().to_ascii_lowercase();
                if normalized.is_empty() {
                    bail!("header target requires a header name");
                }
                let header_name = HeaderName::from_bytes(normalized.as_bytes())
                    .with_context(|| format!("invalid header target name {name}"))?;
                TransformTarget::Header(header_name)
            }
            TransformTargetFile::Cookies {} => TransformTarget::Cookies,
        };

        Ok(Self {
            matcher,
            action,
            target,
            stop: value.stop,
        })
    }
}

pub fn request_url(path: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path.to_owned(),
    }
}

pub fn normalized_content_type(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use rama::http::header;

    #[test]
    fn parses_valid_transform_config() -> anyhow::Result<()> {
        let config: TransformFile = serde_json::from_str(
            r#"{
                "transforms": [
                    {
                        "matcher": {"type": "url_glob", "pattern": "/hello*"},
                        "action": {"type": "replace", "from": "hello", "to": "hi"},
                        "target": {"type": "body"},
                        "stop": true
                    },
                    {
                        "matcher": {"type": "content_type_glob", "pattern": "text/*"},
                        "action": {"type": "replace", "from": "backend", "to": "proxy"},
                        "target": {"type": "all_headers"}
                    }
                ]
            }"#,
        )?;
        let runtime = TransformConfig {
            transforms: config
                .transforms
                .into_iter()
                .map(TransformRule::try_from)
                .collect::<anyhow::Result<Vec<_>>>()?,
        };

        assert_eq!(runtime.transforms.len(), 2);
        Ok(())
    }

    #[test]
    fn rejects_unknown_fields() {
        let result = serde_json::from_str::<TransformFile>(
            r#"{
                "transforms": [
                    {
                        "matcher": {"type": "everything"},
                        "action": {"type": "replace", "from": "a", "to": "b"},
                        "target": {"type": "body"},
                        "unexpected": true
                    }
                ]
            }"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn rejects_header_target_without_name() {
        let result = serde_json::from_str::<TransformFile>(
            r#"{
                "transforms": [
                    {
                        "matcher": {"type": "everything"},
                        "action": {"type": "replace", "from": "a", "to": "b"},
                        "target": {"type": "header"}
                    }
                ]
            }"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn rejects_empty_replace_from() -> anyhow::Result<()> {
        let raw: TransformFile = serde_json::from_str(
            r#"{
                "transforms": [
                    {
                        "matcher": {"type": "everything"},
                        "action": {"type": "replace", "from": "", "to": "b"},
                        "target": {"type": "body"}
                    }
                ]
            }"#,
        )?;

        let rule = raw
            .transforms
            .into_iter()
            .next()
            .context("expected one transform rule")?;
        let result: anyhow::Result<TransformRule> = rule.try_into();
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn matches_url_glob_against_path_and_query() -> anyhow::Result<()> {
        let config = TransformConfig {
            transforms: vec![TransformRule::try_from(TransformRuleFile {
                matcher: TransformMatcherFile::UrlGlob {
                    pattern: "/hello?name=*".to_owned(),
                },
                action: TransformActionFile::Replace {
                    from: "hello".to_owned(),
                    to: "hi".to_owned(),
                },
                target: TransformTargetFile::Body {},
                stop: false,
            })?],
        };
        let headers = HeaderMap::new();

        assert!(config.transforms[0].matches("/hello?name=test", &headers));
        assert!(!config.transforms[0].matches("/goodbye", &headers));
        Ok(())
    }

    #[test]
    fn matches_content_type_without_parameters() -> anyhow::Result<()> {
        let rule = TransformRule::try_from(TransformRuleFile {
            matcher: TransformMatcherFile::ContentTypeGlob {
                pattern: "text/*".to_owned(),
            },
            action: TransformActionFile::Replace {
                from: "a".to_owned(),
                to: "b".to_owned(),
            },
            target: TransformTargetFile::AllHeaders {},
            stop: false,
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );

        assert!(rule.matches("/hello", &headers));
        Ok(())
    }

    #[test]
    fn everything_matcher_always_matches() -> anyhow::Result<()> {
        let rule = TransformRule::try_from(TransformRuleFile {
            matcher: TransformMatcherFile::Everything {},
            action: TransformActionFile::Replace {
                from: "a".to_owned(),
                to: "b".to_owned(),
            },
            target: TransformTargetFile::Body {},
            stop: false,
        })?;

        assert!(rule.matches("/anything", &HeaderMap::new()));
        Ok(())
    }

    #[test]
    fn rewrites_all_headers() -> anyhow::Result<()> {
        let rule = TransformRule::try_from(TransformRuleFile {
            matcher: TransformMatcherFile::Everything {},
            action: TransformActionFile::Replace {
                from: "backend".to_owned(),
                to: "proxy".to_owned(),
            },
            target: TransformTargetFile::AllHeaders {},
            stop: false,
        })?;
        let mut headers = HeaderMap::new();
        headers.insert("x-test", HeaderValue::from_static("backend header"));

        assert!(rule.apply_headers(&mut headers)?);
        assert_eq!(
            headers.get("x-test").and_then(|value| value.to_str().ok()),
            Some("proxy header")
        );
        Ok(())
    }

    #[test]
    fn rewrites_specific_header() -> anyhow::Result<()> {
        let rule = TransformRule::try_from(TransformRuleFile {
            matcher: TransformMatcherFile::Everything {},
            action: TransformActionFile::Replace {
                from: "backend".to_owned(),
                to: "proxy".to_owned(),
            },
            target: TransformTargetFile::Header {
                name: "x-test".to_owned(),
            },
            stop: false,
        })?;
        let mut headers = HeaderMap::new();
        headers.insert("x-test", HeaderValue::from_static("backend header"));
        headers.insert("x-other", HeaderValue::from_static("backend header"));

        assert!(rule.apply_headers(&mut headers)?);
        assert_eq!(
            headers.get("x-test").and_then(|value| value.to_str().ok()),
            Some("proxy header")
        );
        assert_eq!(
            headers.get("x-other").and_then(|value| value.to_str().ok()),
            Some("backend header")
        );
        Ok(())
    }

    #[test]
    fn rewrites_cookies() -> anyhow::Result<()> {
        let rule = TransformRule::try_from(TransformRuleFile {
            matcher: TransformMatcherFile::Everything {},
            action: TransformActionFile::Replace {
                from: "backend".to_owned(),
                to: "proxy".to_owned(),
            },
            target: TransformTargetFile::Cookies {},
            stop: false,
        })?;
        let mut headers = HeaderMap::new();
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("session=backend-token; Path=/"),
        );

        assert!(rule.apply_headers(&mut headers)?);
        assert_eq!(
            headers
                .get(header::SET_COOKIE)
                .and_then(|value| value.to_str().ok()),
            Some("session=proxy-token; Path=/")
        );
        Ok(())
    }

    #[test]
    fn rewrites_body() -> anyhow::Result<()> {
        let rule = TransformRule::try_from(TransformRuleFile {
            matcher: TransformMatcherFile::Everything {},
            action: TransformActionFile::Replace {
                from: "backend".to_owned(),
                to: "proxy".to_owned(),
            },
            target: TransformTargetFile::Body {},
            stop: false,
        })?;
        let mut body = b"hello backend".to_vec();

        let outcome = rule.apply(&mut HeaderMap::new(), &mut body)?;
        assert!(outcome.triggered);
        assert_eq!(body, b"hello proxy");
        Ok(())
    }

    #[test]
    fn stops_processing_after_triggered_stop_rule() -> anyhow::Result<()> {
        let config = TransformConfig {
            transforms: vec![
                TransformRule::try_from(TransformRuleFile {
                    matcher: TransformMatcherFile::Everything {},
                    action: TransformActionFile::Replace {
                        from: "backend".to_owned(),
                        to: "proxy".to_owned(),
                    },
                    target: TransformTargetFile::AllHeaders {},
                    stop: true,
                })?,
                TransformRule::try_from(TransformRuleFile {
                    matcher: TransformMatcherFile::Everything {},
                    action: TransformActionFile::Replace {
                        from: "proxy".to_owned(),
                        to: "final".to_owned(),
                    },
                    target: TransformTargetFile::AllHeaders {},
                    stop: false,
                })?,
            ],
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-test", HeaderValue::from_static("backend header"));

        let outcome = config.apply_until_body("/hello", &mut headers)?;
        assert_eq!(outcome.disposition, PrebufferDisposition::Stream);
        assert_eq!(
            headers.get("x-test").and_then(|value| value.to_str().ok()),
            Some("proxy header")
        );
        Ok(())
    }
}
