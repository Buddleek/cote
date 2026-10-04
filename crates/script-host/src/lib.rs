//! script-host：用户脚本运行时（FR-9）。
//!
//! 引擎为 boa_engine（纯 Rust JS 引擎）：无需 C 工具链 / libclang，
//! windows-gnu 上零构建风险（QuickJS 系需要 bindgen）。
//!
//! 脚本放置于用户配置目录 `scripts/*.js`，自动出现在"脚本"菜单（FR-9.1）。
//! 可用 API（全局 `cote` 对象）：
//!
//! ```js
//! cote.text()                  // -> string  当前文档文本
//! cote.setText(s)              //           整篇替换（UI 侧为单步撤销）
//! cote.selection()             // -> {start, end} | undefined  选区（字符索引）
//! cote.setSelection(start,end) //           设置选区
//! cote.status(msg)             //           状态栏消息
//! ```
//!
//! 隔离（FR-9.2）：
//! - 脚本异常 / 语法错误以 `Err` 返回，不影响主程序；
//! - 循环迭代上限防死循环（见 [`run_script_with_limit`]）；
//! - 不暴露任何文件系统 / 网络能力。
//!
//! 已知限制：无墙钟超时（迭代上限是概率性防护）； boa 解释执行，
//! 大行级脚本性能弱于 QuickJS——对编辑器脚本是可接受的。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, JsArgs, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
};

/// API 版本，供“脚本”菜单展示。
pub const API_VERSION: &str = "0.1";

/// 脚本可访问的编辑器状态（运行前填充，运行后读回）。
#[derive(Debug, Clone, Default)]
pub struct ScriptApi {
    pub text: String,
    /// 选区 (anchor, head)，字符索引
    pub selection: Option<(usize, usize)>,
    pub status: Option<String>,
}

/// 单个已加载脚本。
#[derive(Debug, Clone)]
pub struct ScriptInfo {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
}

/// 默认循环迭代上限（防死循环）。
pub const DEFAULT_LOOP_LIMIT: u64 = 5_000_000;

thread_local! {
    static CURRENT_API: RefCell<Option<Rc<RefCell<ScriptApi>>>> = const { RefCell::new(None) };
}

fn state() -> Rc<RefCell<ScriptApi>> {
    CURRENT_API.with(|c| c.borrow().as_ref().expect("script api state").clone())
}

fn cote_text(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    let s = state().borrow().text.clone();
    Ok(JsValue::from(JsString::from(s)))
}

fn cote_set_text(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = args
        .get_or_undefined(0)
        .to_string(ctx)?
        .to_std_string_escaped();
    state().borrow_mut().text = s;
    Ok(JsValue::undefined())
}

fn cote_status(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = args
        .get_or_undefined(0)
        .to_string(ctx)?
        .to_std_string_escaped();
    state().borrow_mut().status = Some(s);
    Ok(JsValue::undefined())
}

fn cote_selection(_this: &JsValue, _args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let sel = state().borrow().selection;
    let Some((a, h)) = sel else {
        return Ok(JsValue::undefined());
    };
    let obj = JsObject::with_object_proto(ctx.intrinsics());
    obj.set(js_string!("start"), JsValue::from(a as f64), false, ctx)?;
    obj.set(js_string!("end"), JsValue::from(h as f64), false, ctx)?;
    Ok(obj.into())
}

fn cote_set_selection(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let a = args.get_or_undefined(0).to_number(ctx)?.max(0.0) as usize;
    let h = args.get_or_undefined(1).to_number(ctx)?.max(0.0) as usize;
    state().borrow_mut().selection = Some((a, h));
    Ok(JsValue::undefined())
}

/// 执行脚本（默认迭代上限）。传入当前状态，返回执行后的状态。
pub fn run_script(source: &str, api: &ScriptApi) -> Result<ScriptApi, String> {
    run_script_with_limit(source, api, DEFAULT_LOOP_LIMIT)
}

/// 执行脚本并指定循环迭代上限。
pub fn run_script_with_limit(
    source: &str,
    api: &ScriptApi,
    loop_limit: u64,
) -> Result<ScriptApi, String> {
    let holder = Rc::new(RefCell::new(api.clone()));
    let mut context = Context::default();
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(loop_limit);
    CURRENT_API.with(|c| *c.borrow_mut() = Some(holder.clone()));

    // 挂载全局 cote 对象（text/setText/status/selection/setSelection）
    type RawFn = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;
    let realm = context.realm().clone();
    let cote = JsObject::with_object_proto(context.intrinsics());
    let methods: [(&str, RawFn); 5] = [
        ("text", cote_text),
        ("setText", cote_set_text),
        ("status", cote_status),
        ("selection", cote_selection),
        ("setSelection", cote_set_selection),
    ];
    for (name, f) in methods {
        let func = NativeFunction::from_fn_ptr(f).to_js_function(&realm);
        if let Err(e) = cote.set(JsString::from(name), func, false, &mut context) {
            CURRENT_API.with(|c| *c.borrow_mut() = None);
            return Err(e.to_string());
        }
    }
    if let Err(e) = context.register_global_property(js_string!("cote"), cote, Attribute::all()) {
        CURRENT_API.with(|c| *c.borrow_mut() = None);
        return Err(e.to_string());
    }

    let outcome = context.eval(Source::from_bytes(source)).map(|_| ());

    match outcome {
        Ok(()) => {
            let final_api = holder.borrow().clone();
            CURRENT_API.with(|c| *c.borrow_mut() = None);
            Ok(final_api)
        }
        Err(e) => {
            CURRENT_API.with(|c| *c.borrow_mut() = None);
            Err(e.to_string())
        }
    }
}

/// 加载脚本目录下全部 `*.js`（按文件名排序，菜单顺序稳定；文件名即脚本名）。
pub fn load_scripts(dir: &Path) -> Vec<ScriptInfo> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.extension().map(|e| e == "js").unwrap_or(false) {
            if let Ok(source) = std::fs::read_to_string(&p) {
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !name.is_empty() {
                    out.push(ScriptInfo {
                        name,
                        path: p,
                        source,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_transform_roundtrip() {
        let api = ScriptApi {
            text: "hello 中文".to_string(),
            ..Default::default()
        };
        let out = run_script(
            "cote.setText(cote.text().toUpperCase()); cote.status('done');",
            &api,
        )
        .unwrap();
        assert_eq!(out.text, "HELLO 中文");
        assert_eq!(out.status.as_deref(), Some("done"));
        assert_eq!(api.text, "hello 中文"); // 原状态不可变
    }

    #[test]
    fn selection_roundtrip() {
        let api = ScriptApi {
            text: "abc".to_string(),
            selection: Some((5, 5)),
            ..Default::default()
        };
        let out = run_script(
            "const s = cote.selection(); if (s.start !== 5) throw new Error('bad start'); \
             cote.setSelection(0, 3);",
            &api,
        )
        .unwrap();
        assert_eq!(out.selection, Some((0, 3)));
    }

    #[test]
    fn selection_undefined_when_absent() {
        let out = run_script(
            "if (cote.selection() !== undefined) throw new Error('x');",
            &ScriptApi::default(),
        )
        .unwrap();
        assert_eq!(out.selection, None);
    }

    #[test]
    fn script_error_is_isolated() {
        let api = ScriptApi {
            text: "unchanged".to_string(),
            ..Default::default()
        };
        // 运行时异常
        let err = run_script("throw new Error('boom');", &api).unwrap_err();
        assert!(err.contains("boom"));
        // 语法错误
        assert!(run_script("this is not js !!!", &api).is_err());
        // 异常后状态不被污染
        assert_eq!(api.text, "unchanged");
    }

    #[test]
    fn infinite_loop_hits_iteration_limit() {
        let api = ScriptApi::default();
        assert!(run_script_with_limit("while (true) {}", &api, 10_000).is_err());
    }

    #[test]
    fn sandbox_has_no_fs_or_net() {
        let api = ScriptApi::default();
        // require 未定义 → ReferenceError
        assert!(run_script("require;", &api).is_err());
        // 常见 IO / 网络 globals 均不存在
        let out = run_script(
            "if (typeof require !== 'undefined') throw new Error('require exists'); \
             if (typeof fetch !== 'undefined') throw new Error('fetch exists'); \
             if (typeof importScripts !== 'undefined') throw new Error('importScripts exists'); \
             if (typeof process !== 'undefined') throw new Error('process exists');",
            &api,
        );
        assert!(out.is_ok());
    }

    #[test]
    fn load_scripts_from_dir() {
        let dir = std::env::temp_dir().join("cote-script-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("b_upper.js"),
            "cote.setText(cote.text().toUpperCase())",
        )
        .unwrap();
        std::fs::write(dir.join("a_info.js"), "cote.status('info')").unwrap();
        std::fs::write(dir.join("ignore.txt"), "not a script").unwrap();
        let scripts = load_scripts(&dir);
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts[0].name, "a_info"); // 排序稳定
        assert_eq!(scripts[1].name, "b_upper");
        let out = run_script(&scripts[0].source, &ScriptApi::default()).unwrap();
        assert_eq!(out.status.as_deref(), Some("info"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
