use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Number,
    Boolean,
    Date,
    Tags,
    Select(Vec<String>),
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctypeSchema {
    pub name: String,
    pub target_folder: String,
    pub fields: Vec<FieldSchema>,
}
