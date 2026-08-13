use super::super::*;

impl DaemonHandler {
    pub(crate) async fn producer_runtime_observed(
        &self,
        params: Option<Value>,
    ) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            producer: String,
        }
        let params: Params = decode_params(params)?;
        if !self
            .context
            .read()
            .await
            .config
            .producers
            .contains_key(&params.producer)
        {
            return Err(WireError::invalid(format!(
                "unknown producer {:?}",
                params.producer
            )));
        }
        self.append_change(
            ChangeKind::Producer,
            json!({
                "name": params.producer,
                "update": "runtime-observation-recorded",
            }),
        )?;
        Ok(json!({"observed": true}))
    }
}
