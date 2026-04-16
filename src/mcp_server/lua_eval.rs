use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mlua::{Lua, LuaOptions, LuaSerdeExt, MultiValue as LuaMultiValue, StdLib, Value as LuaValue};
use rmcp::model::CallToolResult;
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::daemon;

pub(super) const EVAL_LUA_SCRIPT_NAME: &str = "eval_lua_script";

const CHUNK_NAME: &str = "eval_lua_script";

#[derive(Debug, Deserialize)]
pub(super) struct EvalLuaScriptRequest {
    pub(super) script: String,
    #[serde(default)]
    pub(super) globals: Option<JsonValue>,
}

#[derive(Debug)]
struct LuaExecutionError {
    kind: &'static str,
    message: String,
}

impl LuaExecutionError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[async_trait]
trait LuaMcpToolCaller: Send + Sync {
    async fn call_tool(
        &self,
        mcp_name: &str,
        tool_name: &str,
        arguments: Option<JsonMap<String, JsonValue>>,
    ) -> Result<CallToolResult, String>;
}

struct DaemonLuaMcpToolCaller {
    config_path: PathBuf,
}

#[async_trait]
impl LuaMcpToolCaller for DaemonLuaMcpToolCaller {
    async fn call_tool(
        &self,
        mcp_name: &str,
        tool_name: &str,
        arguments: Option<JsonMap<String, JsonValue>>,
    ) -> Result<CallToolResult, String> {
        daemon::call_tool(&self.config_path, None, mcp_name, tool_name, arguments)
            .await
            .map_err(|error| error.to_string())
    }
}

pub(super) async fn execute_eval_lua_script(
    config_path: &Path,
    request: EvalLuaScriptRequest,
) -> CallToolResult {
    evaluate_lua_script(
        request,
        Arc::new(DaemonLuaMcpToolCaller {
            config_path: config_path.to_path_buf(),
        }),
    )
    .await
}

async fn evaluate_lua_script(
    request: EvalLuaScriptRequest,
    caller: Arc<dyn LuaMcpToolCaller>,
) -> CallToolResult {
    match run_lua_script(request, caller).await {
        Ok(result) => structured_result(json!({ "result": result }), false),
        Err(error) => structured_result(
            json!({
                "error": {
                    "kind": error.kind,
                    "message": error.message,
                }
            }),
            true,
        ),
    }
}

async fn run_lua_script(
    request: EvalLuaScriptRequest,
    caller: Arc<dyn LuaMcpToolCaller>,
) -> Result<JsonValue, LuaExecutionError> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::new()).map_err(|error| {
        LuaExecutionError::new(
            "setup_error",
            format!("failed to initialize Lua VM: {error}"),
        )
    })?;

    register_call_mcp_tool(&lua, caller)?;
    inject_globals(&lua, request.globals)?;

    let values = lua
        .load(&request.script)
        .set_name(CHUNK_NAME)
        .eval_async::<LuaMultiValue>()
        .await
        .map_err(|error| LuaExecutionError::new("script_error", error.to_string()))?;

    lua_multi_value_to_json(&lua, values)
}

fn register_call_mcp_tool(
    lua: &Lua,
    caller: Arc<dyn LuaMcpToolCaller>,
) -> Result<(), LuaExecutionError> {
    let call_mcp_tool = lua
        .create_async_function(
            move |lua, (mcp_name, tool_name, args): (String, String, Option<LuaValue>)| {
                let caller = Arc::clone(&caller);
                async move {
                    if mcp_name.trim().is_empty() {
                        return Err(mlua::Error::runtime("`mcp_name` must not be empty"));
                    }
                    if tool_name.trim().is_empty() {
                        return Err(mlua::Error::runtime("`tool_name` must not be empty"));
                    }

                    let arguments = lua_value_to_tool_args(&lua, args)?;
                    let result = caller
                        .call_tool(&mcp_name, &tool_name, arguments)
                        .await
                        .map_err(mlua::Error::runtime)?;
                    let result_value =
                        serde_json::to_value(&result).map_err(mlua::Error::external)?;

                    lua.to_value(&result_value).map_err(mlua::Error::external)
                }
            },
        )
        .map_err(|error| {
            LuaExecutionError::new(
                "setup_error",
                format!("failed to register `call_mcp_tool`: {error}"),
            )
        })?;

    lua.globals()
        .set("call_mcp_tool", call_mcp_tool)
        .map_err(|error| {
            LuaExecutionError::new(
                "setup_error",
                format!("failed to expose `call_mcp_tool`: {error}"),
            )
        })
}

fn inject_globals(lua: &Lua, globals: Option<JsonValue>) -> Result<(), LuaExecutionError> {
    let Some(globals) = globals else {
        return Ok(());
    };

    let JsonValue::Object(globals) = globals else {
        return Err(LuaExecutionError::new(
            "invalid_globals",
            "`globals` must be a JSON object when provided",
        ));
    };

    if globals.contains_key("call_mcp_tool") {
        return Err(LuaExecutionError::new(
            "invalid_globals",
            "`globals.call_mcp_tool` is reserved",
        ));
    }

    let lua_globals = lua.globals();
    for (name, value) in globals {
        let global_name = name.clone();
        let lua_value = lua.to_value(&value).map_err(|error| {
            LuaExecutionError::new(
                "invalid_globals",
                format!("failed to convert global `{name}` into Lua: {error}"),
            )
        })?;
        lua_globals.set(name, lua_value).map_err(|error| {
            LuaExecutionError::new(
                "invalid_globals",
                format!("failed to assign global `{global_name}` into Lua: {error}"),
            )
        })?;
    }

    Ok(())
}

fn lua_value_to_tool_args(
    lua: &Lua,
    args: Option<LuaValue>,
) -> Result<Option<JsonMap<String, JsonValue>>, mlua::Error> {
    let Some(args) = args else {
        return Ok(None);
    };

    match args {
        LuaValue::Nil => Ok(None),
        value => {
            let json = lua
                .from_value::<JsonValue>(value)
                .map_err(mlua::Error::external)?;
            match json {
                JsonValue::Null => Ok(None),
                JsonValue::Object(map) => Ok(Some(map)),
                _ => Err(mlua::Error::runtime(
                    "`call_mcp_tool` args must decode to a JSON object or nil",
                )),
            }
        }
    }
}

fn lua_multi_value_to_json(
    lua: &Lua,
    values: LuaMultiValue,
) -> Result<JsonValue, LuaExecutionError> {
    match values.len() {
        0 => Ok(JsonValue::Null),
        1 => lua_value_to_json(lua, values.into_iter().next().unwrap()),
        _ => {
            let mut items = Vec::with_capacity(values.len());
            for value in values {
                items.push(lua_value_to_json(lua, value)?);
            }
            Ok(JsonValue::Array(items))
        }
    }
}

fn lua_value_to_json(lua: &Lua, value: LuaValue) -> Result<JsonValue, LuaExecutionError> {
    lua.from_value::<JsonValue>(value).map_err(|error| {
        LuaExecutionError::new(
            "serialization_error",
            format!("failed to serialize Lua result into JSON: {error}"),
        )
    })
}

fn structured_result(payload: JsonValue, is_error: bool) -> CallToolResult {
    let mut result = CallToolResult::structured(payload);
    result.is_error = Some(is_error);
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::model::Content;

    use super::*;

    type RecordedCall = (String, String, Option<JsonMap<String, JsonValue>>);

    #[derive(Default)]
    struct FakeCaller {
        calls: Mutex<Vec<RecordedCall>>,
    }

    #[async_trait]
    impl LuaMcpToolCaller for FakeCaller {
        async fn call_tool(
            &self,
            mcp_name: &str,
            tool_name: &str,
            arguments: Option<JsonMap<String, JsonValue>>,
        ) -> Result<CallToolResult, String> {
            self.calls.lock().unwrap().push((
                mcp_name.to_string(),
                tool_name.to_string(),
                arguments.clone(),
            ));

            let mut result = CallToolResult::success(vec![Content::text("ok")]);
            result.structured_content = Some(json!({
                "server": mcp_name,
                "tool": tool_name,
                "arguments": arguments,
            }));
            result.is_error = Some(false);
            Ok(result)
        }
    }

    #[tokio::test]
    async fn evaluates_basic_lua_expression() {
        let caller: Arc<dyn LuaMcpToolCaller> = Arc::new(FakeCaller::default());
        let result = evaluate_lua_script(
            EvalLuaScriptRequest {
                script: "return 1 + 2".to_string(),
                globals: None,
            },
            caller,
        )
        .await;

        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.structured_content, Some(json!({ "result": 3 })));
    }

    #[tokio::test]
    async fn injects_globals_before_execution() {
        let caller: Arc<dyn LuaMcpToolCaller> = Arc::new(FakeCaller::default());
        let result = evaluate_lua_script(
            EvalLuaScriptRequest {
                script: "return { region = region, count = count }".to_string(),
                globals: Some(json!({
                    "region": "eu",
                    "count": 2,
                })),
            },
            caller,
        )
        .await;

        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content,
            Some(json!({
                "result": {
                    "region": "eu",
                    "count": 2,
                }
            }))
        );
    }

    #[tokio::test]
    async fn lets_lua_call_downstream_mcp_tools() {
        let caller = Arc::new(FakeCaller::default());
        let result = evaluate_lua_script(
            EvalLuaScriptRequest {
                script: r#"
                    local response = call_mcp_tool("context7", "query-docs", {
                        query = "hello",
                    })

                    return {
                        isError = response.isError,
                        text = response.content[1].text,
                        server = response.structuredContent.server,
                        tool = response.structuredContent.tool,
                        query = response.structuredContent.arguments.query,
                    }
                "#
                .to_string(),
                globals: None,
            },
            caller.clone(),
        )
        .await;

        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content,
            Some(json!({
                "result": {
                    "isError": false,
                    "text": "ok",
                    "server": "context7",
                    "tool": "query-docs",
                    "query": "hello",
                }
            }))
        );

        assert_eq!(
            caller.calls.lock().unwrap().as_slice(),
            &[(
                "context7".to_string(),
                "query-docs".to_string(),
                Some(json!({ "query": "hello" }).as_object().unwrap().clone()),
            )]
        );
    }

    #[tokio::test]
    async fn rejects_non_object_tool_args() {
        let caller: Arc<dyn LuaMcpToolCaller> = Arc::new(FakeCaller::default());
        let result = evaluate_lua_script(
            EvalLuaScriptRequest {
                script: "return call_mcp_tool('context7', 'query-docs', { 'hello' })".to_string(),
                globals: None,
            },
            caller,
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.unwrap();
        assert_eq!(payload["error"]["kind"], json!("script_error"));
        assert!(
            payload["error"]["message"]
                .as_str()
                .unwrap()
                .contains("JSON object or nil")
        );
    }

    #[tokio::test]
    async fn returns_structured_error_for_script_failures() {
        let caller: Arc<dyn LuaMcpToolCaller> = Arc::new(FakeCaller::default());
        let result = evaluate_lua_script(
            EvalLuaScriptRequest {
                script: "error('boom')".to_string(),
                globals: None,
            },
            caller,
        )
        .await;

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.unwrap();
        assert_eq!(payload["error"]["kind"], json!("script_error"));
        assert!(
            payload["error"]["message"]
                .as_str()
                .unwrap()
                .contains("boom")
        );
    }
}
