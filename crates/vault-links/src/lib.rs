pub mod walker;
pub mod parser;
pub mod extractor;

pub use extractor::{extract_vault, ExtractionResult};

#[cfg(test)]
mod tests;
