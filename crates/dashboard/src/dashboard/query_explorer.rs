use crate::Tsdb;
use crate::plot::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    // Query Explorer is fully dynamic on the frontend; the backend just
    // returns an empty view (no pre-computed data needed).
    View::new(data, sections)
}
