use crate::data::DashboardData;
use crate::plot::*;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    // Query Explorer is fully dynamic on the frontend; the backend just
    // returns an empty view (no pre-computed data needed).

    View::new(data, sections)
}
