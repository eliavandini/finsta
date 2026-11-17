//! instagram.com's GraphQL operations outside messages (profiles, posts,
//! stories, likes), sent with the same page tokens and operation ids the
//! inbox uses.
//!
//! The website computes some inputs on its side ("provided variables",
//! `__relay_internal__pv__<Name>relayprovider`); they are filled in from
//! the operation's saved definition in `~/.cache/finsta/webcode/`, mostly
//! as `false` (they switch optional fields on).

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::webcode_dir;
use crate::api::Client;

/// Values for provided variables that aren't plain feature switches.
fn provider_value(name: &str) -> Value {
    match name {
        "IGDMaxUnreadMessagesCount" | "IGDInitialMessagePageCount" => json!(20),
        n if n.contains("IsLoggedIn") => json!(true),
        _ => json!(false),
    }
}

/// The saved definition of an operation, if the script search kept it.
fn definition(name: &str) -> Option<String> {
    std::fs::read_to_string(webcode_dir()?.join(format!("{name}.graphql.js"))).ok()
}

/// Provided variable names in a definition.
fn providers(text: &str) -> BTreeSet<String> {
    let re = regex::Regex::new(r"__relay_internal__pv__([A-Za-z0-9_]+?)relayprovider").unwrap();
    re.captures_iter(text).map(|c| c[1].to_string()).collect()
}

/// The inputs an operation declares (its "LocalArgument"s), in order.
fn arguments(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"kind:"LocalArgument",name:"([A-Za-z0-9_]+)""#).unwrap();
    let mut out: Vec<String> = Vec::new();
    for c in re.captures_iter(text) {
        if !out.contains(&c[1].to_string()) {
            out.push(c[1].to_string());
        }
    }
    out
}

/// Add the provided variables an operation's saved definition asks for.
pub(super) fn fill_providers(name: &str, vars: &mut Value) {
    if let Some(text) = definition(name) {
        fill_from(&text, vars);
    }
}

fn fill_from(text: &str, vars: &mut Value) {
    for p in providers(text) {
        let key = format!("__relay_internal__pv__{p}relayprovider");
        if vars.get(&key).is_none_or(Value::is_null) {
            vars[key] = provider_value(&p);
        }
    }
}

/// Shape `vars` to what the saved definition declares: declared inputs that
/// are missing become null, undeclared ones are dropped. Returns the names
/// of both, for the log.
fn fit_arguments(text: &str, vars: &mut Value) -> (Vec<String>, Vec<String>) {
    let declared = arguments(text);
    if declared.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Some(obj) = vars.as_object_mut() else { return (Vec::new(), Vec::new()) };
    let dropped: Vec<String> = obj.keys().filter(|k| !declared.contains(k)).cloned().collect();
    for k in &dropped {
        obj.remove(k);
    }
    let mut missing = Vec::new();
    for d in &declared {
        if !obj.contains_key(d) {
            obj.insert(d.clone(), Value::Null);
            if !d.starts_with("__relay_internal__pv__") {
                missing.push(d.clone());
            }
        }
    }
    (missing, dropped)
}

impl Client {
    /// Run one of instagram.com's GraphQL operations and return its `data`.
    /// Fails when the operation isn't on the website (anymore), or when
    /// Instagram answers with errors and no data.
    pub(crate) async fn web_query(&self, name: &str, mut vars: Value, referer: &str) -> Result<Value> {
        let (cfg, first) = {
            let mut st = self.inner.dm.lock().await;
            if st.config.is_none() {
                self.dm_load_page(&mut st).await?;
            }
            let first = st.logged.insert(format!("query {name}"));
            (st.config.clone().ok_or_else(|| anyhow!("instagram.com didn't load"))?, first)
        };
        let Some(id) = cfg.ops.get(name).cloned() else {
            if first {
                eprintln!("finsta: web: {name} isn't in instagram.com's scripts");
            }
            bail!("instagram.com doesn't offer {name}");
        };
        if let Some(text) = definition(name) {
            let (missing, dropped) = fit_arguments(&text, &mut vars);
            if first && (!missing.is_empty() || !dropped.is_empty()) {
                eprintln!("finsta: web: {name}: inputs set to null {missing:?}; not declared, dropped {dropped:?}");
            }
        } else if first {
            eprintln!("finsta: web: {name}: definition not saved; sending inputs as they are");
        }
        fill_providers(name, &mut vars);
        let answer = self.web_graphql(&cfg, name, &id, &vars, referer).await?;
        let v = answer.json.as_ref();
        let errors = v.is_some_and(|v| v["errors"].as_array().is_some_and(|e| !e.is_empty()) || !v["error"].is_null());
        if first || errors {
            answer.log("web", name);
        }
        let Some(v) = answer.json else {
            bail!("Instagram sent an answer Finsta couldn't read");
        };
        let data = v["data"].as_object().cloned().unwrap_or_else(Map::new);
        let has_data = data.values().any(|x| !x.is_null());
        if !has_data {
            let code = v["errors"][0]["code"].as_i64().or(v["error"].as_i64()).unwrap_or(0);
            bail!("Instagram couldn't do this (error {code})");
        }
        Ok(Value::Object(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEF: &str = r#"__d("X.graphql",[],(function(t,n,r,o,a,i){var e={defaultValue:null,kind:"LocalArgument",name:"media_id"},t={defaultValue:"feed",kind:"LocalArgument",name:"container_module"},l={defaultValue:null,kind:"LocalArgument",name:"__relay_internal__pv__PolarisIsLoggedInrelayprovider"};return{fragment:{argumentDefinitions:[e,t,l]},params:{providedVariables:{__relay_internal__pv__PolarisIsLoggedInrelayprovider:n("PolarisIsLoggedIn.relayprovider"),__relay_internal__pv__PolarisShortDramaEnabledrelayprovider:n("x")}}}}),null);"#;

    #[test]
    fn reads_definitions() {
        assert_eq!(arguments(DEF), ["media_id", "container_module", "__relay_internal__pv__PolarisIsLoggedInrelayprovider"]);
        let p = providers(DEF);
        assert!(p.contains("PolarisIsLoggedIn") && p.contains("PolarisShortDramaEnabled"));
        assert_eq!(provider_value("PolarisIsLoggedIn"), json!(true));
        assert_eq!(provider_value("PolarisShortDramaEnabled"), json!(false));
    }

    #[test]
    fn fits_inputs_to_the_definition() {
        let mut v = json!({"media_id": "1", "extra": 2});
        let (missing, dropped) = fit_arguments(DEF, &mut v);
        assert_eq!(dropped, ["extra"]);
        assert_eq!(missing, ["container_module"]);
        assert_eq!(v["media_id"], "1");
        fill_from(DEF, &mut v);
        assert_eq!(v["__relay_internal__pv__PolarisIsLoggedInrelayprovider"], true);
        assert!(v.get("extra").is_none());
    }
}
