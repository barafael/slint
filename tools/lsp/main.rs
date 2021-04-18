/* LICENSE BEGIN
    This file is part of the SixtyFPS Project -- https://sixtyfps.io
    Copyright (c) 2020 Olivier Goffart <olivier.goffart@sixtyfps.io>
    Copyright (c) 2020 Simon Hausmann <simon.hausmann@sixtyfps.io>

    SPDX-License-Identifier: GPL-3.0-only
    This file is also available under commercial licensing terms.
    Please contact info@sixtyfps.io for more information.
LICENSE END */

mod completion;
mod goto;
mod lsp_ext;
mod preview;
mod util;

use std::collections::HashMap;

use lsp_server::{Connection, Message, Request, RequestId, Response};
use lsp_types::notification::{DidChangeTextDocument, DidOpenTextDocument, Notification};
use lsp_types::request::{CodeActionRequest, ExecuteCommand, GotoDefinition};
use lsp_types::request::{Completion, HoverRequest};
use lsp_types::{
    CodeActionOrCommand, CodeActionProviderCapability, Command, CompletionOptions,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, ExecuteCommandOptions, Hover,
    HoverProviderCapability, InitializeParams, OneOf, Position, PublishDiagnosticsParams, Range,
    ServerCapabilities, TextDocumentSyncCapability, Url, WorkDoneProgressOptions,
};
use sixtyfps_compilerlib::diagnostics::BuildDiagnostics;
use sixtyfps_compilerlib::langtype::Type;
use sixtyfps_compilerlib::parser::{syntax_nodes, SyntaxKind, SyntaxNode, SyntaxToken};
use sixtyfps_compilerlib::typeloader::TypeLoader;
use sixtyfps_compilerlib::typeregister::TypeRegister;
use sixtyfps_compilerlib::CompilerConfiguration;

type Error = Box<dyn std::error::Error>;

const SHOW_PREVIEW_COMMAND: &str = "showPreview";

pub struct DocumentCache<'a> {
    documents: TypeLoader<'a>,
    newline_offsets: HashMap<Url, Vec<u32>>,
}

impl<'a> DocumentCache<'a> {
    fn new(config: &'a CompilerConfiguration) -> Self {
        let documents =
            TypeLoader::new(TypeRegister::builtin(), config, &mut BuildDiagnostics::default());
        Self { documents, newline_offsets: Default::default() }
    }

    fn newline_offsets_from_content(content: &str) -> Vec<u32> {
        let mut ln_offs = 0;
        content
            .split('\n')
            .map(|line| {
                let r = ln_offs;
                ln_offs += line.len() as u32 + 1;
                r
            })
            .collect()
    }
}

fn main() {
    std::thread::spawn(|| {
        match run_lsp_server() {
            Ok(_) => {}
            Err(error) => {
                eprintln!("Error running LSP server: {}", error);
            }
        }
        preview::quit_ui_event_loop();
    });

    preview::start_ui_event_loop();
}

fn run_lsp_server() -> Result<(), Error> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(true),
            trigger_characters: Some(vec![".".to_owned()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            lsp_types::TextDocumentSyncKind::Full,
        )),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![SHOW_PREVIEW_COMMAND.into()],
            ..Default::default()
        }),
        ..ServerCapabilities::default()
    };
    let server_capabilities = serde_json::to_value(&capabilities).unwrap();
    let initialization_params = connection.initialize(server_capabilities)?;
    main_loop(&connection, initialization_params)?;
    io_threads.join()?;
    Ok(())
}

fn main_loop(connection: &Connection, params: serde_json::Value) -> Result<(), Error> {
    let params: InitializeParams = serde_json::from_value(params).unwrap();
    let mut compiler_config = sixtyfps_compilerlib::CompilerConfiguration::new(
        sixtyfps_compilerlib::generator::OutputFormat::Interpreter,
    );
    compiler_config.style = Some("ugly".into());

    let mut document_cache = DocumentCache::new(&compiler_config);
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                handle_request(connection, req, &params, &mut document_cache)?;
            }
            Message::Response(_resp) => {}
            Message::Notification(notifi) => {
                handle_notification(connection, notifi, &mut document_cache)?
            }
        }
    }
    Ok(())
}

fn handle_request(
    connection: &Connection,
    req: Request,
    init_param: &InitializeParams,
    document_cache: &mut DocumentCache,
) -> Result<(), Error> {
    let mut req = Some(req);
    if let Some((id, params)) = cast::<GotoDefinition>(&mut req) {
        let result = token_descr(
            document_cache,
            params.text_document_position_params.text_document,
            params.text_document_position_params.position,
        )
        .and_then(|token| goto::goto_definition(document_cache, token.0));
        let resp = Response::new_ok(id, result);
        connection.sender.send(Message::Response(resp))?;
    } else if let Some((id, params)) = cast::<Completion>(&mut req) {
        let result = token_descr(
            document_cache,
            params.text_document_position.text_document,
            params.text_document_position.position,
        )
        .and_then(|token| {
            completion::completion_at(
                document_cache,
                token.0,
                token.1,
                init_param.capabilities.text_document.as_ref().and_then(|t| t.completion.as_ref()),
            )
        });
        let resp = Response::new_ok(id, result);
        connection.sender.send(Message::Response(resp))?;
    } else if let Some((id, _params)) = cast::<HoverRequest>(&mut req) {
        /*let result =
            token_descr(document_cache, params.text_document_position_params).map(|x| Hover {
                contents: lsp_types::HoverContents::Scalar(MarkedString::from_language_code(
                    "text".into(),
                    format!("{:?}", x.token),
                )),
                range: None,
            });
        let resp = Response::new_ok(id, result);
        connection.sender.send(Message::Response(resp))?;*/
        connection.sender.send(Message::Response(Response::new_ok(id, None::<Hover>)))?;
    } else if let Some((id, params)) = cast::<CodeActionRequest>(&mut req) {
        let result = token_descr(document_cache, params.text_document, params.range.start)
            .and_then(|token| get_code_actions(document_cache, token.0.parent()));
        connection.sender.send(Message::Response(Response::new_ok(id, result)))?;
    } else if let Some((id, params)) = cast::<ExecuteCommand>(&mut req) {
        match params.command.as_str() {
            SHOW_PREVIEW_COMMAND => {
                show_preview_command(&params.arguments, connection, document_cache)?
            }
            _ => (),
        }
        connection
            .sender
            .send(Message::Response(Response::new_ok(id, None::<serde_json::Value>)))?;
    };
    Ok(())
}

fn cast<Kind: lsp_types::request::Request>(
    req: &mut Option<Request>,
) -> Option<(RequestId, Kind::Params)> {
    match req.take().unwrap().extract::<Kind::Params>(Kind::METHOD) {
        Ok(value) => Some(value),
        Err(owned) => {
            *req = Some(owned);
            None
        }
    }
}

fn handle_notification(
    connection: &Connection,
    req: lsp_server::Notification,
    document_cache: &mut DocumentCache,
) -> Result<(), Error> {
    match &*req.method {
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(req.params)?;
            reload_document(
                connection,
                params.text_document.text,
                params.text_document.uri,
                document_cache,
            )?;
        }
        DidChangeTextDocument::METHOD => {
            let mut params: DidChangeTextDocumentParams = serde_json::from_value(req.params)?;
            reload_document(
                connection,
                params.content_changes.pop().unwrap().text,
                params.text_document.uri,
                document_cache,
            )?;
        }
        "sixtyfps/showPreview" => {
            show_preview_command(
                req.params.as_array().map_or(&[], |x| x.as_slice()),
                connection,
                document_cache,
            )?;
        }
        _ => (),
    }
    Ok(())
}

fn show_preview_command(
    params: &[serde_json::Value],
    connection: &Connection,
    document_cache: &DocumentCache,
) -> Result<(), Error> {
    let e = || -> Error { "InvalidParameter".into() };
    let path = if let serde_json::Value::String(s) = params.get(0).ok_or_else(e)? {
        std::path::PathBuf::from(s)
    } else {
        return Err(e());
    };
    let path_canon = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let component = params.get(1).and_then(|v| v.as_str()).map(|v| v.to_string());
    let is_window = component
        .as_ref()
        .and_then(|c| {
            let mut ty = document_cache.documents.get_document(&path)?.local_registry.lookup(&c);
            while let Type::Component(c) = ty {
                ty = c.root_element.borrow().base_type.clone();
            }
            Some(matches!(ty, Type::Builtin(b) if b.name == "Window"))
        })
        .unwrap_or(false);
    preview::load_preview(
        connection.sender.clone(),
        preview::PreviewComponent { path: path_canon.into(), component, is_window },
        preview::PostLoadBehavior::ShowAfterLoad,
    );
    Ok(())
}

fn reload_document(
    connection: &Connection,
    content: String,
    uri: lsp_types::Url,
    document_cache: &mut DocumentCache,
) -> Result<(), Error> {
    let newline_offsets = DocumentCache::newline_offsets_from_content(&content);
    document_cache.newline_offsets.insert(uri.clone(), newline_offsets);

    let path = uri.to_file_path().unwrap();
    let path_canon = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    preview::set_contents(&path_canon, content.clone());
    let mut diag = BuildDiagnostics::default();
    spin_on::spin_on(document_cache.documents.load_file(&path_canon, &path, content, &mut diag));

    // Always provide diagnostics for all files. Empty diagnostics clear any previous ones.
    let mut lsp_diags: HashMap<Url, Vec<lsp_types::Diagnostic>> = core::iter::once(&path)
        .chain(diag.all_loaded_files.iter())
        .map(|path| {
            let uri = Url::from_file_path(path).unwrap();
            (uri, Default::default())
        })
        .collect();

    for d in diag.into_iter() {
        if d.source_file().unwrap().is_relative() {
            continue;
        }
        let uri = Url::from_file_path(d.source_file().unwrap()).unwrap();
        lsp_diags.entry(uri).or_default().push(to_lsp_diag(&d));
    }

    for (uri, diagnostics) in lsp_diags {
        connection.sender.send(Message::Notification(lsp_server::Notification::new(
            "textDocument/publishDiagnostics".into(),
            PublishDiagnosticsParams { uri, diagnostics, version: None },
        )))?;
    }

    Ok(())
}

fn to_lsp_diag_level(
    level: sixtyfps_compilerlib::diagnostics::DiagnosticLevel,
) -> lsp_types::DiagnosticSeverity {
    match level {
        sixtyfps_interpreter::DiagnosticLevel::Error => lsp_types::DiagnosticSeverity::Error,
        sixtyfps_interpreter::DiagnosticLevel::Warning => lsp_types::DiagnosticSeverity::Warning,
    }
}

fn to_lsp_diag(d: &sixtyfps_compilerlib::diagnostics::Diagnostic) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic::new(
        to_range(d.line_column()),
        Some(to_lsp_diag_level(d.level())),
        None,
        None,
        d.message().to_owned(),
        None,
        None,
    )
}

fn to_range(span: (usize, usize)) -> Range {
    let pos = Position::new((span.0 as u32).saturating_sub(1), (span.1 as u32).saturating_sub(1));
    Range::new(pos, pos)
}

/// return the token, and the offset within the file
fn token_descr(
    document_cache: &mut DocumentCache,
    text_document: lsp_types::TextDocumentIdentifier,
    pos: Position,
) -> Option<(SyntaxToken, u32)> {
    let o = document_cache.newline_offsets.get(&text_document.uri)?.get(pos.line as usize)?
        + pos.character as u32;

    let doc = document_cache.documents.get_document(&text_document.uri.to_file_path().ok()?)?;
    let node = doc.node.as_ref()?;
    if !node.text_range().contains(o.into()) {
        return None;
    }
    let mut taf = node.token_at_offset(o.into());
    let token = match (taf.next(), taf.next()) {
        (None, _) => return None,
        (Some(t), None) => t,
        (Some(l), Some(r)) => match (l.kind(), r.kind()) {
            // Prioritize identifier
            (SyntaxKind::Identifier, _) => l,
            (_, SyntaxKind::Identifier) => r,
            // then the dot
            (SyntaxKind::Dot, _) => l,
            (_, SyntaxKind::Dot) => r,
            // de-prioritize the white spaces
            (SyntaxKind::Whitespace, _) => r,
            (SyntaxKind::Comment, _) => r,
            (_, SyntaxKind::Whitespace) => l,
            (_, SyntaxKind::Comment) => l,
            _ => l,
        },
    };
    Some((SyntaxToken { token, source_file: node.source_file.clone() }, o))
}

fn get_code_actions(
    _document_cache: &mut DocumentCache,
    node: SyntaxNode,
) -> Option<Vec<CodeActionOrCommand>> {
    let component = syntax_nodes::Component::new(node.clone())
        .or_else(|| {
            syntax_nodes::DeclaredIdentifier::new(node.clone())
                .and_then(|n| n.parent())
                .and_then(|p| syntax_nodes::Component::new(p))
        })
        .or_else(|| {
            syntax_nodes::QualifiedName::new(node.clone())
                .and_then(|n| n.parent())
                .and_then(|p| syntax_nodes::Element::new(p))
                .and_then(|n| n.parent())
                .and_then(|p| syntax_nodes::Component::new(p))
        })?;

    let component_name =
        sixtyfps_compilerlib::parser::identifier_text(&component.DeclaredIdentifier())?;

    Some(vec![CodeActionOrCommand::Command(Command::new(
        "Show preview".into(),
        SHOW_PREVIEW_COMMAND.into(),
        Some(vec![component.source_file.path().to_string_lossy().into(), component_name.into()]),
    ))])
}
