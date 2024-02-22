/*
 * Copyright 2024 Oxide Computer Company
 */

use std::{io::IsTerminal, sync::Mutex};

use slog::{o, Drain, Logger};

pub fn make_log(name: &'static str) -> Logger {
    if std::io::stdout().is_terminal() {
        /*
         * Use a terminal-formatted logger for interactive processes.
         */
        let dec = slog_term::TermDecorator::new().stdout().build();
        let dr = Mutex::new(
            slog_term::FullFormat::new(dec).use_original_order().build(),
        )
        .filter_level(slog::Level::Debug)
        .fuse();
        Logger::root(dr, o!("name" => name))
    } else {
        /*
         * Otherwise, emit bunyan-formatted records:
         */
        slog::Logger::root(
            Mutex::new(
                slog_bunyan::with_name(name, std::io::stdout())
                    .set_flush(true)
                    .build(),
            )
            .fuse(),
            o!(),
        )
    }
}
