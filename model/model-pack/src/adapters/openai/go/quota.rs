use anyhow::{Context, Result, ensure};
use proteus_contracts::contracts::{
    ModelQuotaAmount, ModelQuotaBucket, ModelQuotaSnapshot, ModelQuotaWindow,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Meter {
    status: String,
    percent: f64,
    #[serde(rename = "resetsAt")]
    resets_at: String,
}
#[derive(Deserialize)]
struct Usage {
    rolling: Meter,
    weekly: Meter,
    monthly: Meter,
}
#[derive(Deserialize)]
struct Report {
    usage: Usage,
}

fn timestamp(s: &str) -> Result<u64> {
    let t = chrono::DateTime::parse_from_rfc3339(s)?.timestamp();
    ensure!(t > 0, "invalid reset time");
    Ok(t as u64)
}

pub(super) fn usage(body: &[u8], observed_at: u64) -> Result<ModelQuotaSnapshot> {
    let Report { usage } = serde_json::from_slice(body).context("invalid OpenCode quota")?;
    let mut limited = false;
    let mut windows = Vec::new();
    for (id, meter, duration_seconds) in [
        ("rolling", usage.rolling, Some(18000)),
        ("weekly", usage.weekly, Some(604800)),
        ("monthly", usage.monthly, None),
    ] {
        ensure!(
            matches!(meter.status.as_str(), "ok" | "rate-limited")
                && meter.percent.is_finite()
                && meter.percent >= 0.0,
            "invalid OpenCode meter"
        );
        limited |= meter.status == "rate-limited";
        windows.push(ModelQuotaWindow {
            id: id.into(),
            used_percent: meter.percent,
            duration_seconds,
            resets_at: Some(timestamp(&meter.resets_at)?),
            amount: None,
        });
    }
    Ok(ModelQuotaSnapshot {
        observed_at,
        plan: Some("OpenCode Go".into()),
        credits: None,
        notices: vec![],
        buckets: vec![ModelQuotaBucket {
            id: "opencode-go".into(),
            name: Some("OpenCode Go · общий лимит".into()),
            allowed: Some(!limited),
            limit_reached: Some(limited),
            windows,
            informational: false,
        }],
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    #[serde(rename = "ruleID")]
    rule_id: String,
    limit_dollars: f64,
    spent_dollars: f64,
    scope: String,
    per: String,
    coverage: String,
    models: Vec<BudgetModel>,
    vendors: Vec<Vendor>,
    resets_at: String,
    input_price_at_least: Option<f64>,
    shared: bool,
    picked: bool,
}
#[derive(Deserialize)]
struct BudgetModel {
    #[serde(rename = "providerID")]
    provider: String,
    #[serde(rename = "modelID")]
    id: String,
    name: String,
    #[serde(rename = "spentDollars")]
    spent: Option<f64>,
}
#[derive(Deserialize)]
struct Vendor {
    id: String,
    name: String,
}
pub(super) fn budgets(bytes: &[u8]) -> Result<Vec<ModelQuotaBucket>> {
    let rules: Vec<Rule> = serde_json::from_slice(bytes).context("invalid OpenCode budgets")?;
    let mut buckets = vec![];
    let mut ids = std::collections::HashSet::new();
    for rule in rules {
        let _flags = (rule.shared, rule.picked);
        ensure!(
            !rule.rule_id.trim().is_empty()
                && ids.insert(rule.rule_id.clone())
                && rule.limit_dollars >= 0.
                && rule.spent_dollars >= 0.
                && rule.input_price_at_least.is_none_or(|x| x >= 0.)
                && matches!(rule.per.as_str(), "model" | "set")
                && matches!(rule.coverage.as_str(), "all" | "models"),
            "invalid OpenCode budget rule"
        );
        let scope = match rule.scope.as_str() {
            "combined" => "Общий бюджет",
            "each" => "Личный бюджет",
            _ => anyhow::bail!("invalid budget scope"),
        };
        let reset = timestamp(&rule.resets_at)?;
        let mut model_ids = std::collections::HashSet::new();
        ensure!(
            rule.models.iter().all(|m| !m.id.is_empty()
                && !m.provider.is_empty()
                && !m.name.is_empty()
                && model_ids.insert((&m.provider, &m.id))
                && m.spent.is_none_or(|v| v >= 0.))
                && rule
                    .vendors
                    .iter()
                    .all(|v| !v.id.is_empty() && !v.name.is_empty()),
            "invalid budget coverage"
        );
        let mut add = |id: String, name: String, used: f64| -> Result<()> {
            let limit = rule.limit_dollars;
            let pct = if limit == 0. {
                100.
            } else {
                used / limit * 100.
            };
            ensure!(pct.is_finite(), "invalid budget percentage");
            buckets.push(ModelQuotaBucket {
                id,
                name: Some(name),
                allowed: None,
                limit_reached: None,
                informational: true,
                windows: vec![ModelQuotaWindow {
                    id: "budget".into(),
                    used_percent: pct,
                    duration_seconds: None,
                    resets_at: Some(reset),
                    amount: Some(ModelQuotaAmount {
                        used,
                        limit,
                        unit: "USD".into(),
                    }),
                }],
            });
            Ok(())
        };
        if rule.per == "model" {
            for m in &rule.models {
                add(
                    format!("budget:{}:{}/{}", rule.rule_id, m.provider, m.id),
                    format!("{scope} · {}", m.name),
                    m.spent.context("missing per-model spend")?,
                )?;
            }
        } else {
            let names = rule
                .models
                .iter()
                .map(|m| m.name.as_str())
                .chain(rule.vendors.iter().map(|v| v.name.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            let coverage = if rule.coverage == "all" {
                "все платные модели и сервисы"
            } else if names.is_empty() {
                "выбранные модели"
            } else {
                &names
            };
            let mut label = format!("{scope} · {coverage}");
            if let Some(price) = rule.input_price_at_least {
                label.push_str(&format!(" · вход от ${price}/млн"));
            }
            add(
                format!("budget:{}", rule.rule_id),
                label,
                rule.spent_dollars,
            )?;
        }
    }
    Ok(buckets)
}
