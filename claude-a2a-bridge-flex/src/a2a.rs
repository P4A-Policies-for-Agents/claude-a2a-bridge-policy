// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Sliced A2A schema types used to deserialize inbound `message/send` params.
//!
//! Only the exact type closure the policy references is defined here:
//! `MessageSendParams`, `Message`, `Part` (untagged), `TextPart`, `FilePart`,
//! `DataPart`, `FileContent`, and `Role`. Conversion/`FromStr`/`Display` impls
//! and unrelated A2A schema types are intentionally omitted.

pub const MESSAGE_SEND_FUNCTION_NAME: &str = "message/send";

pub mod schemas {
    // A2A 0.2.3 specific structures
    #[doc = "MessageSendParams for A2A 0.2.3"]
    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    pub struct MessageSendParams {
        pub message: Message,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub metadata:
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    }

    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    pub struct Message {
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub metadata:
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        pub parts: ::std::vec::Vec<Part>,
        pub role: Role,
    }

    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    #[serde(untagged)]
    pub enum Part {
        TextPart(TextPart),
        FilePart(FilePart),
        DataPart(DataPart),
    }

    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    pub struct TextPart {
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub kind: ::std::option::Option<::std::string::String>,
        #[serde(rename = "type", default, skip_serializing_if = "::std::option::Option::is_none")]
        pub type_: ::std::option::Option<::std::string::String>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub metadata:
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        pub text: ::std::string::String,
    }

    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    pub struct FilePart {
        pub file: FileContent,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub metadata:
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub kind: ::std::option::Option<::std::string::String>,
        #[doc = "Type of the part"]
        #[serde(rename = "type", default, skip_serializing_if = "::std::option::Option::is_none")]
        pub type_: ::std::option::Option<::std::string::String>,
    }

    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    pub struct DataPart {
        pub data: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub metadata:
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub kind: ::std::option::Option<::std::string::String>,
        #[doc = "Type of the part"]
        #[serde(rename = "type", default, skip_serializing_if = "::std::option::Option::is_none")]
        pub type_: ::std::option::Option<::std::string::String>,
    }

    #[doc = "Represents the content of a file, either as base64 encoded bytes or a URI.\n\nEnsures that either 'bytes' or 'uri' is provided, but not both."]
    #[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
    pub struct FileContent {
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub bytes: ::std::option::Option<::std::string::String>,
        #[serde(
            rename = "mimeType",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        pub mime_type: ::std::option::Option<::std::string::String>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub name: ::std::option::Option<::std::string::String>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        pub uri: ::std::option::Option<::std::string::String>,
    }

    #[derive(
        :: serde :: Deserialize,
        :: serde :: Serialize,
        Clone,
        Copy,
        Debug,
        Eq,
        Hash,
        Ord,
        PartialEq,
        PartialOrd,
    )]
    pub enum Role {
        #[serde(rename = "user")]
        User,
        #[serde(rename = "agent")]
        Agent,
    }
}
