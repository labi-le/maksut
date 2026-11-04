use crate::*;

/// One successful completion response: the assistant message plus the usage
/// block the provider billed for it.
pub(crate) struct Completion {
    pub(crate) message: Value,
    pub(crate) usage: Option<Usage>,
}

/// The title generator's output; usage survives even when the reply is empty,
/// because the request was still billed.
pub(crate) struct TitleResult {
    pub(crate) title: Result<String, String>,
    pub(crate) usage: Option<Usage>,
}

/// Everything that shapes one completion request.
#[derive(Clone, Copy)]
pub(crate) struct CompletionRequest<'a> {
    pub(crate) messages: &'a [Value],
    pub(crate) owner: bool,
    pub(crate) feature_tools: bool,
    pub(crate) max_tokens: u32,
    pub(crate) research_mode: bool,
    pub(crate) allow_sticker: bool,
    pub(crate) allow_feature: bool,
    pub(crate) reasoning: bool,
    pub(crate) temperature: f64,
}

impl<'a> CompletionRequest<'a> {
    pub(crate) fn new(messages: &'a [Value], owner: bool, max_tokens: u32) -> Self {
        Self {
            messages,
            owner,
            feature_tools: false,
            max_tokens,
            research_mode: false,
            allow_sticker: false,
            allow_feature: false,
            reasoning: true,
            temperature: 0.7,
        }
    }
    pub(crate) fn without_reasoning(mut self) -> Self {
        self.reasoning = false;
        self
    }
    pub(crate) fn tool_mode(&self) -> ToolMode {
        ToolMode {
            owner: self.owner,
            feature_tools: self.feature_tools,
            research_mode: self.research_mode,
            allow_sticker: self.allow_sticker,
            allow_feature: self.allow_feature,
        }
    }
    pub(crate) fn feature_tools(mut self, enabled: bool) -> Self {
        self.feature_tools = enabled;
        self
    }
    pub(crate) fn research_mode(mut self, enabled: bool) -> Self {
        self.research_mode = enabled;
        self
    }
    pub(crate) fn allow_sticker(mut self, enabled: bool) -> Self {
        self.allow_sticker = enabled;
        self
    }
    pub(crate) fn allow_feature(mut self, enabled: bool) -> Self {
        self.allow_feature = enabled;
        self
    }
}

#[derive(Debug)]
pub(crate) enum CompleteError {
    Rejected,
    Timeout,
    Status(u16),
    Failed,
}

impl CompleteError {
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Timeout => "timeout".into(),
            Self::Status(code) => format!("http_{code}"),
            Self::Rejected => "rejected".into(),
            Self::Failed => "model".into(),
        }
    }
}
