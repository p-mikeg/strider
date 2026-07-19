//! Fallible operations return [`Result`] (= [`anyhow::Result<T>`]).

pub type Result<T> = anyhow::Result<T>;
