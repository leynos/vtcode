//! Downstream compile-pass guards for the renamed catalogue APIs.
//!
//! The `diapason` package consumes the three libraries that own these names,
//! so this integration test proves each replacement is reachable from a
//! downstream crate through its public path rather than only from the
//! defining crate's own tests.

use diapason_config::models::ModelCatalogueEntry;
use diapason_core::ToolCatalogue;
use diapason_core::llm::ResolvedModel;
use diapason_core::tools::registry::ToolCatalogueSource;

#[test]
fn replacement_catalogue_names_are_reachable_downstream() {
    fn _accepts_trait_object(_catalogue: &dyn ToolCatalogue) {}
    fn _accepts_source(_source: ToolCatalogueSource) {}
    fn _accepts_entry(_entry: ModelCatalogueEntry) {}
    fn _reads_catalogue(model: &ResolvedModel) -> Option<ModelCatalogueEntry> {
        model.catalogue
    }
}
