//! The client against `scripts/fake-lsp` (needs `python3`).

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anveg_lsp::lsp_types::{self, notification, request};
use anveg_lsp::{LanguageServer, ServerCommand, incremental_change, path_to_uri};
use anyhow::{Result, anyhow};

fn fake() -> Option<PathBuf> {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fake-lsp");
    let python = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    python.then_some(script)
}

#[test]
fn talks_to_a_server() -> Result<()> {
    let Some(script) = fake() else {
        eprintln!("python3 not found; skipping");
        return Ok(());
    };
    let tmp = std::env::temp_dir();
    let (tx, rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let server = LanguageServer::spawn(
        &ServerCommand {
            program: PathBuf::from("python3"),
            args: vec![script.display().to_string()],
            cwd: tmp.clone(),
            env: None,
        },
        move |n| {
            let _ = tx.send(n);
        },
        move || {
            let _ = exit_tx.send(());
        },
    )?;
    futures::executor::block_on(server.initialize(std::slice::from_ref(&tmp)))?;
    assert!(server.incremental_sync());

    let path = tmp.join("x.txt");
    let uri = path_to_uri(&path).ok_or_else(|| anyhow!("uri"))?;
    let text = "one\ntwo bad\n";
    server.notify::<notification::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: lsp_types::TextDocumentItem {
            uri: uri.clone(),
            language_id: "plaintext".into(),
            version: 1,
            text: text.into(),
        },
    })?;
    let n = rx.recv_timeout(Duration::from_secs(5))?;
    assert_eq!(n.method, anveg_lsp::methods::PUBLISH_DIAGNOSTICS);
    let p: lsp_types::PublishDiagnosticsParams = serde_json::from_value(n.params)?;
    assert_eq!(p.diagnostics.len(), 1);
    assert_eq!(p.diagnostics[0].range.start.line, 1);

    // An incremental edit that removes the bad word.
    let new = "one\ntwo good\nbad three\n";
    server.notify::<notification::DidChangeTextDocument>(lsp_types::DidChangeTextDocumentParams {
        text_document: lsp_types::VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: 2,
        },
        content_changes: vec![incremental_change(text, new)],
    })?;
    let p: lsp_types::PublishDiagnosticsParams =
        serde_json::from_value(rx.recv_timeout(Duration::from_secs(5))?.params)?;
    assert_eq!(p.diagnostics.len(), 1);
    assert_eq!(p.diagnostics[0].range.start.line, 2);

    let position = lsp_types::TextDocumentPositionParams {
        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        position: lsp_types::Position::new(0, 1),
    };
    let completion = futures::executor::block_on(server.request::<request::Completion>(
        lsp_types::CompletionParams {
            text_document_position: position.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        },
    ))?;
    let labels: Vec<String> = match completion {
        Some(lsp_types::CompletionResponse::Array(items)) => {
            items.into_iter().map(|i| i.label).collect()
        }
        Some(lsp_types::CompletionResponse::List(l)) => {
            l.items.into_iter().map(|i| i.label).collect()
        }
        None => Vec::new(),
    };
    assert_eq!(labels, ["alpha", "beta"]);

    let hover = futures::executor::block_on(server.request::<request::HoverRequest>(
        lsp_types::HoverParams {
            text_document_position_params: position,
            work_done_progress_params: Default::default(),
        },
    ))?;
    assert!(hover.is_some());

    // An unknown request fails cleanly.
    let err = futures::executor::block_on(server.request::<request::References>(
        lsp_types::ReferenceParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: lsp_types::Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp_types::ReferenceContext {
                include_declaration: false,
            },
        },
    ));
    assert!(err.is_err());

    futures::executor::block_on(server.shutdown());
    exit_rx.recv_timeout(Duration::from_secs(5))?;
    assert!(!server.is_running());
    Ok(())
}
