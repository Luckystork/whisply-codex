use std::convert::Infallible;
use whisply_protocol::openai_models::ModelPreset;

#[derive(Debug, Clone)]
pub(crate) struct ModelCatalog {
    models: Vec<ModelPreset>,
}

impl ModelCatalog {
    pub(crate) fn new(models: Vec<ModelPreset>) -> Self {
        Self { models }
    }

    pub(crate) fn try_list_models(&self) -> Result<Vec<ModelPreset>, Infallible> {
        Ok(self.models.clone())
    }
}
