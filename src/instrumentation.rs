use anyhow::Result;

pub struct KeyValue;

impl KeyValue {
    pub fn new<T>(_key: &str, _value: T) -> Self {
        Self
    }
}

pub fn with_span<T>(_name: &str, _attrs: Vec<KeyValue>, f: impl FnOnce() -> T) -> T {
    f()
}

pub fn with_span_result<T>(
    _name: &str,
    _attrs: Vec<KeyValue>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    f()
}

pub fn record_error_message(_message: &str) {}
