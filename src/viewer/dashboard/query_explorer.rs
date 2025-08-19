use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    // Query Explorer doesn't need pre-computed data, it's all dynamic

    // We could add some example queries or metadata here if needed
    // For now, just return an empty view as the frontend will handle everything

    View::new(data, sections)
}
