//! Strict strategy markdown parsing, validation, and compilation.

use std::collections::{BTreeSet, HashMap};

use serde::de::DeserializeOwned;
use tv_bot_core_types::{
    BrokerPreference, CompiledStrategy, DashboardDisplay, DataRequirements, EntryRules,
    ExecutionSpec, ExitRules, FailsafeRules, FlattenRuleMode, MarketConfig, PositionSizing,
    PositionSizingMode, ReversalMode, RiskLimits, SessionMode, SessionRules, SignalCombinationMode,
    SignalConfirmation, StateBehavior, StrategyMetadata, TradeManagement, WarmupRequirements,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SectionKey {
    Metadata,
    Market,
    Session,
    DataRequirements,
    Warmup,
    SignalConfirmation,
    EntryRules,
    ExitRules,
    PositionSizing,
    Execution,
    TradeManagement,
    Risk,
    Failsafes,
    StateBehavior,
    DashboardDisplay,
}

impl SectionKey {
    fn title(self) -> &'static str {
        match self {
            Self::Metadata => "Metadata",
            Self::Market => "Market",
            Self::Session => "Session",
            Self::DataRequirements => "Data Requirements",
            Self::Warmup => "Warmup",
            Self::SignalConfirmation => "Signal Confirmation",
            Self::EntryRules => "Entry Rules",
            Self::ExitRules => "Exit Rules",
            Self::PositionSizing => "Position Sizing",
            Self::Execution => "Execution",
            Self::TradeManagement => "Trade Management",
            Self::Risk => "Risk",
            Self::Failsafes => "Failsafes",
            Self::StateBehavior => "State Behavior",
            Self::DashboardDisplay => "Dashboard Display",
        }
    }

    fn from_heading(heading: &str) -> Option<Self> {
        match heading.trim() {
            "Metadata" => Some(Self::Metadata),
            "Market" => Some(Self::Market),
            "Session" => Some(Self::Session),
            "Data Requirements" => Some(Self::DataRequirements),
            "Warmup" => Some(Self::Warmup),
            "Signal Confirmation" => Some(Self::SignalConfirmation),
            "Entry Rules" => Some(Self::EntryRules),
            "Exit Rules" => Some(Self::ExitRules),
            "Position Sizing" => Some(Self::PositionSizing),
            "Execution" => Some(Self::Execution),
            "Trade Management" => Some(Self::TradeManagement),
            "Risk" => Some(Self::Risk),
            "Failsafes" => Some(Self::Failsafes),
            "State Behavior" => Some(Self::StateBehavior),
            "Dashboard Display" => Some(Self::DashboardDisplay),
            _ => None,
        }
    }

    fn all() -> &'static [SectionKey] {
        &[
            Self::Metadata,
            Self::Market,
            Self::Session,
            Self::DataRequirements,
            Self::Warmup,
            Self::SignalConfirmation,
            Self::EntryRules,
            Self::ExitRules,
            Self::PositionSizing,
            Self::Execution,
            Self::TradeManagement,
            Self::Risk,
            Self::Failsafes,
            Self::StateBehavior,
            Self::DashboardDisplay,
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrategyIssueSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyIssue {
    pub severity: StrategyIssueSeverity,
    pub message: String,
    pub section: Option<String>,
    pub field: Option<String>,
    pub line: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrategyCompilation {
    pub title: Option<String>,
    pub compiled: CompiledStrategy,
    pub warnings: Vec<StrategyIssue>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StrategyCompileError {
    pub errors: Vec<StrategyIssue>,
    pub warnings: Vec<StrategyIssue>,
}

impl std::fmt::Display for StrategyCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "strategy compilation failed with {} error(s)",
            self.errors.len()
        )
    }
}

impl std::error::Error for StrategyCompileError {}

pub trait StrategyCompilerService {
    fn compile(&self, markdown: &str) -> Result<StrategyCompilation, StrategyCompileError>;
}

#[derive(Clone, Debug, Default)]
pub struct StrictStrategyCompiler;

impl StrictStrategyCompiler {
    pub fn compile_markdown(
        &self,
        markdown: &str,
    ) -> Result<StrategyCompilation, StrategyCompileError> {
        StrategyCompilerService::compile(self, markdown)
    }
}

impl StrategyCompilerService for StrictStrategyCompiler {
    fn compile(&self, markdown: &str) -> Result<StrategyCompilation, StrategyCompileError> {
        let parsed = parse_markdown(markdown)?;

        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        let metadata =
            parse_section::<StrategyMetadata>(&parsed, SectionKey::Metadata, &mut errors);
        let market = parse_section::<MarketConfig>(&parsed, SectionKey::Market, &mut errors);
        let session = parse_section::<SessionRules>(&parsed, SectionKey::Session, &mut errors);
        let data_requirements =
            parse_section::<DataRequirements>(&parsed, SectionKey::DataRequirements, &mut errors);
        let warmup = parse_section::<WarmupRequirements>(&parsed, SectionKey::Warmup, &mut errors);
        let signal_confirmation = parse_section::<SignalConfirmation>(
            &parsed,
            SectionKey::SignalConfirmation,
            &mut errors,
        );
        let entry_rules = parse_section::<EntryRules>(&parsed, SectionKey::EntryRules, &mut errors);
        let exit_rules = parse_section::<ExitRules>(&parsed, SectionKey::ExitRules, &mut errors);
        let position_sizing =
            parse_section::<PositionSizing>(&parsed, SectionKey::PositionSizing, &mut errors);
        let execution = parse_section::<ExecutionSpec>(&parsed, SectionKey::Execution, &mut errors);
        let trade_management =
            parse_section::<TradeManagement>(&parsed, SectionKey::TradeManagement, &mut errors);
        let risk = parse_section::<RiskLimits>(&parsed, SectionKey::Risk, &mut errors);
        let failsafes = parse_section::<FailsafeRules>(&parsed, SectionKey::Failsafes, &mut errors);
        let state_behavior =
            parse_section::<StateBehavior>(&parsed, SectionKey::StateBehavior, &mut errors);
        let dashboard_display =
            parse_section::<DashboardDisplay>(&parsed, SectionKey::DashboardDisplay, &mut errors);

        if !errors.is_empty() {
            return Err(StrategyCompileError { errors, warnings });
        }

        let metadata = metadata.expect("metadata must exist");
        let market = market.expect("market must exist");
        let session = session.expect("session must exist");
        let data_requirements = data_requirements.expect("data requirements must exist");
        let warmup = warmup.expect("warmup must exist");
        let signal_confirmation = signal_confirmation.expect("signal confirmation must exist");
        let entry_rules = entry_rules.expect("entry rules must exist");
        let exit_rules = exit_rules.expect("exit rules must exist");
        let position_sizing = position_sizing.expect("position sizing must exist");
        let execution = execution.expect("execution must exist");
        let trade_management = trade_management.expect("trade management must exist");
        let risk = risk.expect("risk must exist");
        let failsafes = failsafes.expect("failsafes must exist");
        let state_behavior = state_behavior.expect("state behavior must exist");
        let dashboard_display = dashboard_display.expect("dashboard display must exist");

        validate_metadata(&metadata, &mut errors);
        validate_market(&market, &mut errors);
        validate_session(&session, &mut errors);
        validate_data_requirements(&data_requirements, &mut errors);
        validate_warmup(&warmup, &data_requirements, &mut errors);
        validate_signal_confirmation(&signal_confirmation, &mut errors);
        validate_entry_rules(&entry_rules, &mut errors);
        validate_exit_rules(&exit_rules, &mut errors);
        validate_position_sizing(&position_sizing, &mut errors);
        validate_execution(&execution, &mut errors, &mut warnings);
        validate_trade_management(&trade_management, &mut errors);
        validate_risk(&risk, &mut errors);
        validate_dashboard_display(&dashboard_display, &mut warnings);

        if !errors.is_empty() {
            return Err(StrategyCompileError { errors, warnings });
        }

        Ok(StrategyCompilation {
            title: parsed.title,
            compiled: CompiledStrategy {
                metadata,
                market,
                session,
                data_requirements,
                warmup,
                signal_confirmation,
                entry_rules,
                exit_rules,
                position_sizing,
                execution,
                trade_management,
                risk,
                failsafes,
                state_behavior,
                dashboard_display,
            },
            warnings,
        })
    }
}

#[derive(Clone, Debug)]
struct ParsedDocument {
    title: Option<String>,
    sections: HashMap<SectionKey, SectionBlock>,
}

#[derive(Clone, Debug)]
struct SectionBlock {
    yaml: String,
    line: usize,
}

fn parse_markdown(markdown: &str) -> Result<ParsedDocument, StrategyCompileError> {
    let mut title = None;
    let mut sections = HashMap::new();
    let mut errors = Vec::new();
    let mut lines = markdown.lines().enumerate().peekable();

    while let Some((line_number, line)) = lines.next() {
        let trimmed = line.trim();

        if title.is_none() && trimmed.starts_with("# ") {
            title = Some(trimmed.trim_start_matches("# ").trim().to_owned());
            continue;
        }

        let Some(heading) = trimmed.strip_prefix("## ") else {
            continue;
        };

        let Some(section_key) = SectionKey::from_heading(heading) else {
            errors.push(error(
                Some(heading.to_owned()),
                None,
                Some(line_number + 1),
                format!("unknown strategy section `{heading}`"),
            ));
            continue;
        };

        if sections.contains_key(&section_key) {
            errors.push(error(
                Some(section_key.title().to_owned()),
                None,
                Some(line_number + 1),
                "duplicate strategy section".to_owned(),
            ));
            continue;
        }

        let mut yaml: Option<String> = None;
        let mut code_block_count = 0usize;

        while let Some((next_line_number, next_line)) = lines.peek().copied() {
            let next_trimmed = next_line.trim();

            if next_trimmed.starts_with("## ") {
                break;
            }

            lines.next();

            if next_trimmed == "```yaml" || next_trimmed == "```yml" {
                code_block_count += 1;

                if code_block_count > 1 {
                    errors.push(error(
                        Some(section_key.title().to_owned()),
                        None,
                        Some(next_line_number + 1),
                        "each section must contain exactly one YAML block".to_owned(),
                    ));
                }

                let mut buffer = Vec::new();
                let mut closed = false;

                while let Some((_code_line_number, code_line)) = lines.next() {
                    if code_line.trim() == "```" {
                        closed = true;
                        break;
                    }

                    buffer.push(code_line.trim_end_matches('\r').to_owned());
                }

                if !closed {
                    errors.push(error(
                        Some(section_key.title().to_owned()),
                        None,
                        Some(next_line_number + 1),
                        "unterminated YAML code block".to_owned(),
                    ));
                    break;
                }

                if yaml.is_none() {
                    yaml = Some(buffer.join("\n"));
                }
            }
        }

        if code_block_count == 0 {
            errors.push(error(
                Some(section_key.title().to_owned()),
                None,
                Some(line_number + 1),
                "required section is missing a YAML code block".to_owned(),
            ));
            continue;
        }

        if let Some(yaml) = yaml {
            sections.insert(
                section_key,
                SectionBlock {
                    yaml,
                    line: line_number + 1,
                },
            );
        }
    }

    for section in SectionKey::all() {
        if !sections.contains_key(section) {
            errors.push(error(
                Some(section.title().to_owned()),
                None,
                None,
                "required section is missing".to_owned(),
            ));
        }
    }

    if errors.is_empty() {
        Ok(ParsedDocument { title, sections })
    } else {
        Err(StrategyCompileError {
            errors,
            warnings: Vec::new(),
        })
    }
}

fn parse_section<T: DeserializeOwned>(
    parsed: &ParsedDocument,
    key: SectionKey,
    errors: &mut Vec<StrategyIssue>,
) -> Option<T> {
    let block = parsed
        .sections
        .get(&key)
        .expect("required sections are checked during parsing");

    match serde_yaml::from_str::<T>(&block.yaml) {
        Ok(value) => Some(value),
        Err(source) => {
            errors.push(error(
                Some(key.title().to_owned()),
                None,
                Some(block.line),
                format!("invalid YAML or schema mismatch: {source}"),
            ));
            None
        }
    }
}

fn error(
    section: Option<String>,
    field: Option<String>,
    line: Option<usize>,
    message: String,
) -> StrategyIssue {
    StrategyIssue {
        severity: StrategyIssueSeverity::Error,
        message,
        section,
        field,
        line,
    }
}

fn warning(
    section: Option<String>,
    field: Option<String>,
    line: Option<usize>,
    message: String,
) -> StrategyIssue {
    StrategyIssue {
        severity: StrategyIssueSeverity::Warning,
        message,
        section,
        field,
        line,
    }
}

fn validate_metadata(metadata: &StrategyMetadata, errors: &mut Vec<StrategyIssue>) {
    if metadata.schema_version != 1 {
        errors.push(error(
            Some("Metadata".to_owned()),
            Some("schema_version".to_owned()),
            None,
            "V1 compiler only supports schema_version: 1".to_owned(),
        ));
    }

    if metadata.strategy_id.trim().is_empty() || !is_identifier_friendly(&metadata.strategy_id) {
        errors.push(error(
            Some("Metadata".to_owned()),
            Some("strategy_id".to_owned()),
            None,
            "strategy_id must use only ASCII letters, numbers, underscores, or hyphens".to_owned(),
        ));
    }

    for (field, value) in [
        ("name", metadata.name.trim()),
        ("version", metadata.version.trim()),
        ("author", metadata.author.trim()),
        ("description", metadata.description.trim()),
    ] {
        if value.is_empty() {
            errors.push(error(
                Some("Metadata".to_owned()),
                Some(field.to_owned()),
                None,
                "required field cannot be empty".to_owned(),
            ));
        }
    }
}

fn validate_market(market: &MarketConfig, errors: &mut Vec<StrategyIssue>) {
    if market.market.trim().is_empty() {
        errors.push(error(
            Some("Market".to_owned()),
            Some("market".to_owned()),
            None,
            "market must not be empty".to_owned(),
        ));
    }
}

fn validate_session(session: &SessionRules, errors: &mut Vec<StrategyIssue>) {
    if session.timezone.trim().is_empty() {
        errors.push(error(
            Some("Session".to_owned()),
            Some("timezone".to_owned()),
            None,
            "timezone must not be empty".to_owned(),
        ));
    }

    match session.mode {
        SessionMode::Always => {}
        SessionMode::FixedWindow => match &session.trade_window {
            Some(window) => {
                validate_time("Session", "trade_window.start", &window.start, errors);
                validate_time("Session", "trade_window.end", &window.end, errors);
            }
            None => errors.push(error(
                Some("Session".to_owned()),
                Some("trade_window".to_owned()),
                None,
                "fixed_window mode requires trade_window".to_owned(),
            )),
        },
    }

    if let Some(time) = &session.no_new_entries_after {
        validate_time("Session", "no_new_entries_after", time, errors);
    }

    if let Some(flatten_rule) = &session.flatten_rule {
        match flatten_rule.mode {
            FlattenRuleMode::ByTime => {
                if let Some(time) = &flatten_rule.time {
                    validate_time("Session", "flatten_rule.time", time, errors);
                } else {
                    errors.push(error(
                        Some("Session".to_owned()),
                        Some("flatten_rule.time".to_owned()),
                        None,
                        "flatten_rule.time is required when mode is by_time".to_owned(),
                    ));
                }
            }
            FlattenRuleMode::None | FlattenRuleMode::SessionEnd => {}
        }
    }
}

fn validate_data_requirements(
    data_requirements: &DataRequirements,
    errors: &mut Vec<StrategyIssue>,
) {
    if data_requirements.feeds.is_empty() {
        errors.push(error(
            Some("Data Requirements".to_owned()),
            Some("feeds".to_owned()),
            None,
            "at least one feed is required".to_owned(),
        ));
    }

    if data_requirements.timeframes.is_empty() {
        errors.push(error(
            Some("Data Requirements".to_owned()),
            Some("timeframes".to_owned()),
            None,
            "at least one timeframe is required".to_owned(),
        ));
    }

    if !data_requirements.multi_timeframe && data_requirements.timeframes.len() > 1 {
        errors.push(error(
            Some("Data Requirements".to_owned()),
            Some("multi_timeframe".to_owned()),
            None,
            "multi_timeframe must be true when more than one timeframe is requested".to_owned(),
        ));
    }

    if has_duplicates(&data_requirements.timeframes) {
        errors.push(error(
            Some("Data Requirements".to_owned()),
            Some("timeframes".to_owned()),
            None,
            "timeframes must not contain duplicates".to_owned(),
        ));
    }
}

fn validate_warmup(
    warmup: &WarmupRequirements,
    data_requirements: &DataRequirements,
    errors: &mut Vec<StrategyIssue>,
) {
    if warmup.bars_required.is_empty() {
        errors.push(error(
            Some("Warmup".to_owned()),
            Some("bars_required".to_owned()),
            None,
            "warmup bars_required must not be empty".to_owned(),
        ));
    }

    for (timeframe, bars) in &warmup.bars_required {
        if *bars == 0 {
            errors.push(error(
                Some("Warmup".to_owned()),
                Some("bars_required".to_owned()),
                None,
                format!("warmup bars_required for {timeframe:?} must be greater than zero"),
            ));
        }

        if !data_requirements.timeframes.contains(timeframe) {
            errors.push(error(
                Some("Warmup".to_owned()),
                Some("bars_required".to_owned()),
                None,
                "warmup cannot reference timeframes that are not declared in data requirements"
                    .to_owned(),
            ));
        }
    }

    if warmup.ready_requires_all {
        for timeframe in &data_requirements.timeframes {
            if !warmup.bars_required.contains_key(timeframe) {
                errors.push(error(
                    Some("Warmup".to_owned()),
                    Some("bars_required".to_owned()),
                    None,
                    "ready_requires_all demands bars_required for every declared timeframe"
                        .to_owned(),
                ));
            }
        }
    }
}

fn validate_signal_confirmation(
    signal_confirmation: &SignalConfirmation,
    errors: &mut Vec<StrategyIssue>,
) {
    if signal_confirmation.primary_conditions.is_empty() {
        errors.push(error(
            Some("Signal Confirmation".to_owned()),
            Some("primary_conditions".to_owned()),
            None,
            "at least one primary condition is required".to_owned(),
        ));
    }

    match signal_confirmation.mode {
        SignalCombinationMode::All | SignalCombinationMode::Any => {}
        SignalCombinationMode::NOfM => match signal_confirmation.n_required {
            Some(required) if required > 0 => {
                if required as usize > signal_confirmation.primary_conditions.len() {
                    errors.push(error(
                        Some("Signal Confirmation".to_owned()),
                        Some("n_required".to_owned()),
                        None,
                        "n_required cannot exceed the number of primary_conditions".to_owned(),
                    ));
                }
            }
            _ => errors.push(error(
                Some("Signal Confirmation".to_owned()),
                Some("n_required".to_owned()),
                None,
                "n_of_m mode requires n_required > 0".to_owned(),
            )),
        },
        SignalCombinationMode::WeightedScore => {
            if signal_confirmation.score_threshold.is_none() {
                errors.push(error(
                    Some("Signal Confirmation".to_owned()),
                    Some("score_threshold".to_owned()),
                    None,
                    "weighted_score mode requires score_threshold".to_owned(),
                ));
            }
        }
    }
}

fn validate_entry_rules(entry_rules: &EntryRules, errors: &mut Vec<StrategyIssue>) {
    if !entry_rules.long_enabled && !entry_rules.short_enabled {
        errors.push(error(
            Some("Entry Rules".to_owned()),
            None,
            None,
            "at least one side must be enabled for entry".to_owned(),
        ));
    }

    if matches!(entry_rules.max_entry_distance_ticks, Some(0))
        || matches!(entry_rules.entry_timeout_seconds, Some(0))
    {
        errors.push(error(
            Some("Entry Rules".to_owned()),
            None,
            None,
            "entry tick distance and timeout values must be greater than zero when provided"
                .to_owned(),
        ));
    }
}

fn validate_exit_rules(exit_rules: &ExitRules, errors: &mut Vec<StrategyIssue>) {
    if matches!(exit_rules.max_hold_seconds, Some(0)) {
        errors.push(error(
            Some("Exit Rules".to_owned()),
            Some("max_hold_seconds".to_owned()),
            None,
            "max_hold_seconds must be greater than zero when provided".to_owned(),
        ));
    }
}

fn validate_position_sizing(position_sizing: &PositionSizing, errors: &mut Vec<StrategyIssue>) {
    match position_sizing.mode {
        PositionSizingMode::Fixed => match position_sizing.contracts {
            Some(contracts) if contracts > 0 => {}
            _ => errors.push(error(
                Some("Position Sizing".to_owned()),
                Some("contracts".to_owned()),
                None,
                "fixed sizing requires contracts > 0".to_owned(),
            )),
        },
        PositionSizingMode::RiskBased => {
            if position_sizing.max_risk_usd.is_none() {
                errors.push(error(
                    Some("Position Sizing".to_owned()),
                    Some("max_risk_usd".to_owned()),
                    None,
                    "risk_based sizing requires max_risk_usd".to_owned(),
                ));
            }

            if matches!(position_sizing.fallback_fixed_contracts, Some(0))
                || matches!(position_sizing.min_contracts, Some(0))
                || matches!(position_sizing.max_contracts, Some(0))
            {
                errors.push(error(
                    Some("Position Sizing".to_owned()),
                    None,
                    None,
                    "contract counts must be greater than zero when provided".to_owned(),
                ));
            }

            if let (Some(min), Some(max)) =
                (position_sizing.min_contracts, position_sizing.max_contracts)
            {
                if min > max {
                    errors.push(error(
                        Some("Position Sizing".to_owned()),
                        None,
                        None,
                        "min_contracts cannot exceed max_contracts".to_owned(),
                    ));
                }
            }
        }
    }
}

fn validate_execution(
    execution: &ExecutionSpec,
    errors: &mut Vec<StrategyIssue>,
    warnings: &mut Vec<StrategyIssue>,
) {
    if execution.scaling.max_legs == 0 {
        errors.push(error(
            Some("Execution".to_owned()),
            Some("scaling.max_legs".to_owned()),
            None,
            "max_legs must be at least 1".to_owned(),
        ));
    }

    if !execution.scaling.allow_scale_in
        && !execution.scaling.allow_scale_out
        && execution.scaling.max_legs > 1
    {
        errors.push(error(
            Some("Execution".to_owned()),
            Some("scaling.max_legs".to_owned()),
            None,
            "max_legs cannot exceed 1 when scaling is disabled".to_owned(),
        ));
    }

    if execution.scaling.max_legs == 1
        && (execution.scaling.allow_scale_in || execution.scaling.allow_scale_out)
    {
        warnings.push(warning(
            Some("Execution".to_owned()),
            Some("scaling.max_legs".to_owned()),
            None,
            "scaling flags are enabled but max_legs is 1, so scaling has no effect".to_owned(),
        ));
    }

    if execution.reversal_mode == ReversalMode::DirectReverse {
        warnings.push(warning(
            Some("Execution".to_owned()),
            Some("reversal_mode".to_owned()),
            None,
            "direct_reverse should be paper-tested carefully before live use".to_owned(),
        ));
    }

    if execution.broker_preferences.trailing_stop == BrokerPreference::BrokerPreferred {
        warnings.push(warning(
            Some("Execution".to_owned()),
            Some("broker_preferences.trailing_stop".to_owned()),
            None,
            "broker_preferred trailing stops require paper validation before relying on them"
                .to_owned(),
        ));
    }
}

fn validate_trade_management(trade_management: &TradeManagement, errors: &mut Vec<StrategyIssue>) {
    if trade_management.initial_stop_ticks == 0 || trade_management.take_profit_ticks == 0 {
        errors.push(error(
            Some("Trade Management".to_owned()),
            None,
            None,
            "initial_stop_ticks and take_profit_ticks must both be greater than zero".to_owned(),
        ));
    }

    if let Some(rule) = &trade_management.break_even {
        if rule.enabled && !matches!(rule.activate_at_ticks, Some(value) if value > 0) {
            errors.push(error(
                Some("Trade Management".to_owned()),
                Some("break_even.activate_at_ticks".to_owned()),
                None,
                "break_even requires activate_at_ticks > 0 when enabled".to_owned(),
            ));
        }
    }

    if let Some(rule) = &trade_management.trailing {
        let valid_activate = matches!(rule.activate_at_ticks, Some(value) if value > 0);
        let valid_trail = matches!(rule.trail_ticks, Some(value) if value > 0);

        if rule.enabled && (!valid_activate || !valid_trail) {
            errors.push(error(
                Some("Trade Management".to_owned()),
                Some("trailing".to_owned()),
                None,
                "trailing requires activate_at_ticks > 0 and trail_ticks > 0 when enabled"
                    .to_owned(),
            ));
        }
    }

    if let Some(rule) = &trade_management.partial_take_profit {
        if rule.enabled {
            if rule.targets.is_empty() {
                errors.push(error(
                    Some("Trade Management".to_owned()),
                    Some("partial_take_profit.targets".to_owned()),
                    None,
                    "partial_take_profit requires at least one target when enabled".to_owned(),
                ));
            }

            let mut percent_total: u32 = 0;
            for target in &rule.targets {
                if target.at_ticks == 0 || target.percent == 0 {
                    errors.push(error(
                        Some("Trade Management".to_owned()),
                        Some("partial_take_profit.targets".to_owned()),
                        None,
                        "partial take-profit targets must use positive tick and percent values"
                            .to_owned(),
                    ));
                }
                percent_total += u32::from(target.percent);
            }

            if percent_total > 100 {
                errors.push(error(
                    Some("Trade Management".to_owned()),
                    Some("partial_take_profit.targets".to_owned()),
                    None,
                    "partial take-profit percentages cannot exceed 100".to_owned(),
                ));
            }
        }
    }
}

fn validate_risk(risk: &RiskLimits, errors: &mut Vec<StrategyIssue>) {
    if risk.max_trades_per_day == 0 {
        errors.push(error(
            Some("Risk".to_owned()),
            Some("max_trades_per_day".to_owned()),
            None,
            "max_trades_per_day must be greater than zero".to_owned(),
        ));
    }

    if risk.max_consecutive_losses == 0 {
        errors.push(error(
            Some("Risk".to_owned()),
            Some("max_consecutive_losses".to_owned()),
            None,
            "max_consecutive_losses must be greater than zero".to_owned(),
        ));
    }

    if matches!(risk.max_open_positions, Some(0)) {
        errors.push(error(
            Some("Risk".to_owned()),
            Some("max_open_positions".to_owned()),
            None,
            "max_open_positions must be greater than zero when provided".to_owned(),
        ));
    }
}

fn validate_dashboard_display(
    dashboard_display: &DashboardDisplay,
    warnings: &mut Vec<StrategyIssue>,
) {
    if dashboard_display.show.is_empty() {
        warnings.push(warning(
            Some("Dashboard Display".to_owned()),
            Some("show".to_owned()),
            None,
            "dashboard display should list at least one panel".to_owned(),
        ));
    }

    let allowed_show = [
        "pnl",
        "net_pnl",
        "fills",
        "active_brackets",
        "latency",
        "health",
        "orders",
        "positions",
    ];
    let allowed_debug = ["signal_state", "sizing", "risk_preview", "latency"];

    for panel in &dashboard_display.show {
        if !allowed_show.contains(&panel.as_str()) {
            warnings.push(warning(
                Some("Dashboard Display".to_owned()),
                Some("show".to_owned()),
                None,
                format!("unknown display panel `{panel}` will be treated as a hint only"),
            ));
        }
    }

    for panel in &dashboard_display.debug_panels {
        if !allowed_debug.contains(&panel.as_str()) {
            warnings.push(warning(
                Some("Dashboard Display".to_owned()),
                Some("debug_panels".to_owned()),
                None,
                format!("unknown debug panel `{panel}` will be treated as a hint only"),
            ));
        }
    }
}

fn validate_time(section: &str, field: &str, value: &str, errors: &mut Vec<StrategyIssue>) {
    if !is_valid_hh_mm_ss(value) {
        errors.push(error(
            Some(section.to_owned()),
            Some(field.to_owned()),
            None,
            "time values must use HH:MM:SS format".to_owned(),
        ));
    }
}

fn is_valid_hh_mm_ss(value: &str) -> bool {
    let parts: Vec<_> = value.split(':').collect();
    if parts.len() != 3 {
        return false;
    }

    let Ok(hours) = parts[0].parse::<u8>() else {
        return false;
    };
    let Ok(minutes) = parts[1].parse::<u8>() else {
        return false;
    };
    let Ok(seconds) = parts[2].parse::<u8>() else {
        return false;
    };

    hours < 24 && minutes < 60 && seconds < 60
}

fn is_identifier_friendly(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

fn has_duplicates<T: Ord + Clone>(items: &[T]) -> bool {
    let mut seen = BTreeSet::new();
    items.iter().cloned().any(|item| !seen.insert(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiler() -> StrictStrategyCompiler {
        StrictStrategyCompiler
    }

    fn valid_strategy() -> &'static str {
        r#"
# Strategy: GC Momentum Fade

## Metadata
```yaml
schema_version: 1
strategy_id: gc_momentum_fade_v1
name: GC Momentum Fade
version: 1.0.0
author: internal
description: Fade setup for front-month gold futures
```

## Market
```yaml
market: gold
selection:
  contract_mode: front_month_auto
```

## Session
```yaml
mode: fixed_window
timezone: America/New_York
trade_window:
  start: "08:30:00"
  end: "11:30:00"
flatten_rule:
  mode: by_time
  time: "13:00:00"
```

## Data Requirements
```yaml
feeds:
  - type: trades
  - type: ohlcv_1s
timeframes:
  - 1s
  - 1m
  - 5m
multi_timeframe: true
requires:
  volume: true
```

## Warmup
```yaml
bars_required:
  "1s": 600
  "1m": 100
  "5m": 50
ready_requires_all: true
```

## Signal Confirmation
```yaml
mode: all
primary_conditions:
  - trend_filter
  - rejection
  - volume_gate
```

## Entry Rules
```yaml
long_enabled: true
short_enabled: true
entry_order_type: market
```

## Exit Rules
```yaml
exit_on_opposite_signal: false
flatten_on_session_end: true
```

## Position Sizing
```yaml
mode: risk_based
max_risk_usd: 250
fallback_fixed_contracts: 1
```

## Execution
```yaml
reversal_mode: flatten_first
scaling:
  allow_scale_in: true
  allow_scale_out: true
  max_legs: 3
broker_preferences:
  stop_loss: broker_required
  take_profit: broker_required
  trailing_stop: broker_preferred
```

## Trade Management
```yaml
initial_stop_ticks: 40
take_profit_ticks: 80
break_even:
  enabled: true
  activate_at_ticks: 30
trailing:
  enabled: true
  activate_at_ticks: 50
  trail_ticks: 20
```

## Risk
```yaml
daily_loss:
  broker_side_required: true
  local_backup_enabled: true
max_trades_per_day: 6
max_consecutive_losses: 3
max_open_positions: 1
```

## Failsafes
```yaml
no_new_entries_on_data_degrade: true
pause_on_broker_sync_mismatch: true
pause_on_reconnect_until_reviewed: true
```

## State Behavior
```yaml
cooldown_after_loss_s: 300
max_reentries_per_side: 2
```

## Dashboard Display
```yaml
show:
  - pnl
  - net_pnl
  - fills
  - active_brackets
  - latency
  - health
default_overlay: entries_exits
debug_panels:
  - signal_state
  - sizing
  - risk_preview
```
"#
    }

    #[test]
    fn compiles_valid_strategy_into_runtime_object() {
        let compilation = compiler()
            .compile(valid_strategy())
            .expect("strategy should compile");

        assert_eq!(
            compilation.compiled.metadata.strategy_id,
            "gc_momentum_fade_v1"
        );
        assert_eq!(compilation.compiled.data_requirements.timeframes.len(), 3);
        assert_eq!(
            compilation.compiled.execution.reversal_mode,
            ReversalMode::FlattenFirst
        );
        assert_eq!(compilation.warnings.len(), 1);
    }

    #[test]
    fn missing_required_section_fails_validation() {
        let strategy = valid_strategy().replace("## Risk", "## Removed");
        let error = compiler()
            .compile(&strategy)
            .expect_err("missing risk section should fail");

        assert!(error
            .errors
            .iter()
            .any(|issue| issue.message.contains("required section is missing")));
    }

    #[test]
    fn unknown_section_fails() {
        let strategy = format!(
            "{}\n## Surprise Section\n```yaml\nvalue: true\n```\n",
            valid_strategy()
        );
        let error = compiler()
            .compile(&strategy)
            .expect_err("unknown section should fail");

        assert!(error
            .errors
            .iter()
            .any(|issue| issue.message.contains("unknown strategy section")));
    }

    #[test]
    fn unknown_fields_fail_validation() {
        let strategy = valid_strategy().replace(
            "entry_order_type: market",
            "entry_order_type: market\nmade_up_flag: true",
        );
        let error = compiler()
            .compile(&strategy)
            .expect_err("unknown fields should fail");

        assert!(error
            .errors
            .iter()
            .any(|issue| issue.message.contains("schema mismatch")));
    }

    #[test]
    fn impossible_warmup_configuration_fails() {
        let strategy = valid_strategy().replace(
            "  \"1m\": 100\n  \"5m\": 50\nready_requires_all: true",
            "ready_requires_all: true",
        );
        let error = compiler()
            .compile(&strategy)
            .expect_err("warmup mismatch should fail");

        assert!(error.errors.iter().any(|issue| {
            issue
                .message
                .contains("ready_requires_all demands bars_required")
        }));
    }

    #[test]
    fn invalid_time_format_fails() {
        let strategy = valid_strategy().replace("\"08:30:00\"", "\"8:30\"");
        let error = compiler()
            .compile(&strategy)
            .expect_err("invalid time format should fail");

        assert!(error
            .errors
            .iter()
            .any(|issue| issue.message.contains("HH:MM:SS")));
    }
}
