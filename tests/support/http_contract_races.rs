use super::{Harness, TestResult};
use serde_json::{Value, json};

impl Harness {
    fn race_branch<'a>(&self, step: &'a Value) -> TestResult<&'a Value> {
        let current = json!(self.race_winners);
        let branches = step["branches"].as_array().ok_or("race branches missing")?;
        let matches: Vec<_> = branches
            .iter()
            .filter(|b| b["when"]["race_winners"] == current)
            .collect();
        if matches.len() != 1 {
            return Err(format!(
                "{} / {}: expected one branch for {current}, found {}",
                self.name,
                step["label"],
                matches.len()
            ));
        }
        Ok(matches[0])
    }

    pub(super) async fn run_conditional_step(&mut self, step: &Value) -> TestResult<()> {
        let selected = self.race_branch(step)?["step"].clone();
        match selected["op"].as_str() {
            Some("http") => self.run_http_step(&selected).await,
            other => Err(format!(
                "{}: unsupported conditional operation {other:?}",
                self.name
            )),
        }
    }

    pub(super) async fn run_race_step(&mut self, step: &Value) -> TestResult<()> {
        let branch = self.race_branch(step)?.clone();
        let requests = branch["requests"]
            .as_array()
            .ok_or("race requests missing")?;
        if requests.len() != 2 || requests[0]["label"] == requests[1]["label"] {
            return Err("a frozen race must have two uniquely labelled requests".into());
        }
        // Both HTTP futures are polled together. Accept one complete captured
        // outcome, never a combination of individually successful responses.
        let (left, right) = tokio::join!(
            self.send_request_immutable(&requests[0]["request"], false),
            self.send_request_immutable(&requests[1]["request"], false),
        );
        let responses = [left?, right?];
        let allowed = branch["allowed_results"]
            .as_array()
            .ok_or("race results missing")?;
        let mut matching = Vec::new();
        let mut differences = Vec::new();
        for outcome in allowed {
            let expected = outcome["responses"]
                .as_array()
                .ok_or("race response set missing")?;
            if expected.len() != requests.len() {
                return Err("frozen race response count differs from its request count".into());
            }
            let mut valid = true;
            for (request, response) in requests.iter().zip(&responses) {
                let candidates: Vec<_> = expected
                    .iter()
                    .filter(|e| e["label"] == request["label"])
                    .collect();
                if candidates.len() != 1 {
                    return Err("frozen race response labels are not one-to-one".into());
                }
                if let Err(error) = self.compare_response(candidates[0], response, false) {
                    valid = false;
                    differences.push(error);
                }
            }
            if valid {
                matching.push(outcome);
            }
        }
        if matching.len() != 1 {
            return Err(format!(
                "{} / {}: matched {} complete frozen race outcomes; {}",
                self.name,
                step["label"],
                matching.len(),
                differences.join("; ")
            ));
        }
        let outcome = matching[0];
        let winner = outcome["winner"].as_str().ok_or("race winner missing")?;
        if winner != "idempotent-replay" {
            let label = step["label"].as_str().ok_or("race label missing")?;
            if self
                .race_winners
                .insert(label.to_owned(), winner.to_owned())
                .is_some()
            {
                return Err("race winner was assigned twice".into());
            }
        }
        let mut upstream = Vec::new();
        for (request, response) in requests.iter().zip(&responses) {
            let expected = outcome["responses"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["label"] == request["label"])
                .unwrap();
            self.bind_http_aliases(expected, &response.body).await?;
            self.bind_clock_aliases_from_response(expected, &response.body)?;
            self.record_signature(&request["request"], response)?;
            upstream.extend(
                expected["upstream_calls"]
                    .as_array()
                    .ok_or("race upstream calls missing")?
                    .iter()
                    .cloned(),
            );
        }
        self.compare_upstream_calls(&json!({"label":step["label"],"upstream_calls":upstream}))
            .await?;
        Ok(())
    }
}
