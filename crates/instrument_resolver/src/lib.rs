//! Front-month contract and provider symbol resolution.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use thiserror::Error;
use tv_bot_core_types::{
    CompiledStrategy, ContractMode, ContractMonth, DatabentoInstrument, DatabentoSymbology,
    FrontMonthSelectionBasis, FuturesContract, InstrumentMapping,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketDefinition {
    pub market_family: &'static str,
    pub display_name: &'static str,
    pub venue: &'static str,
    pub symbol_root: &'static str,
    pub tradovate_symbol_root: &'static str,
    pub databento_dataset: &'static str,
    pub aliases: &'static [&'static str],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractListing {
    pub month: ContractMonth,
    pub databento_symbol: String,
    pub tradovate_symbol: String,
    pub first_notice_date: Option<NaiveDate>,
    pub last_trade_date: Option<NaiveDate>,
}

impl ContractListing {
    pub fn new(
        month: ContractMonth,
        databento_symbol: impl Into<String>,
        tradovate_symbol: impl Into<String>,
    ) -> Self {
        Self {
            month,
            databento_symbol: databento_symbol.into(),
            tradovate_symbol: tradovate_symbol.into(),
            first_notice_date: None,
            last_trade_date: None,
        }
    }

    pub fn with_first_notice_date(mut self, first_notice_date: NaiveDate) -> Self {
        self.first_notice_date = Some(first_notice_date);
        self
    }

    pub fn with_last_trade_date(mut self, last_trade_date: NaiveDate) -> Self {
        self.last_trade_date = Some(last_trade_date);
        self
    }

    fn rollover_cutoff(&self) -> Option<(NaiveDate, FrontMonthSelectionBasis)> {
        self.first_notice_date
            .map(|date| (date, FrontMonthSelectionBasis::FirstNoticeDate))
            .or_else(|| {
                self.last_trade_date
                    .map(|date| (date, FrontMonthSelectionBasis::LastTradeDate))
            })
    }
}

pub trait ContractChainProvider {
    fn contract_chain(
        &self,
        market: &MarketDefinition,
    ) -> Result<Vec<ContractListing>, ContractChainError>;
}

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractChainError {
    #[error("contract chain unavailable for market family `{market}`")]
    Unavailable { market: String },
    #[error("contract chain provider error for market family `{market}`: {message}")]
    Provider { market: String, message: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InstrumentResolverError {
    #[error("unsupported market family `{market}`")]
    UnsupportedMarket { market: String },
    #[error(transparent)]
    ContractChain(#[from] ContractChainError),
    #[error("contract chain for `{market}` is empty")]
    EmptyContractChain { market: String },
    #[error("contract chain for `{market}` contains duplicate contract months")]
    DuplicateContractMonth { market: String },
    #[error("contract chain for `{market}` contains an invalid contract month `{month}`")]
    InvalidContractMonth { market: String, month: u8 },
    #[error("no active front-month contract could be resolved for `{market}` as of {as_of}")]
    NoActiveContract { market: String, as_of: NaiveDate },
}

pub struct FrontMonthResolver<P, C = SystemClock> {
    provider: P,
    clock: C,
}

impl<P> FrontMonthResolver<P, SystemClock>
where
    P: ContractChainProvider,
{
    pub fn with_system_clock(provider: P) -> Self {
        Self {
            provider,
            clock: SystemClock,
        }
    }
}

impl<P, C> FrontMonthResolver<P, C>
where
    P: ContractChainProvider,
    C: Clock,
{
    pub fn new(provider: P, clock: C) -> Self {
        Self { provider, clock }
    }

    pub fn resolve_for_strategy(
        &self,
        strategy: &CompiledStrategy,
    ) -> Result<InstrumentMapping, InstrumentResolverError> {
        self.resolve_market(
            &strategy.market.market,
            strategy.market.selection.contract_mode.clone(),
        )
    }

    pub fn resolve_market(
        &self,
        market_family: &str,
        contract_mode: ContractMode,
    ) -> Result<InstrumentMapping, InstrumentResolverError> {
        let resolved_at = self.clock.now();
        let market = lookup_market_definition(market_family).ok_or_else(|| {
            InstrumentResolverError::UnsupportedMarket {
                market: market_family.to_owned(),
            }
        })?;

        match contract_mode {
            ContractMode::FrontMonthAuto => {
                self.resolve_front_month(market, contract_mode, resolved_at)
            }
        }
    }

    fn resolve_front_month(
        &self,
        market: MarketDefinition,
        contract_mode: ContractMode,
        resolved_at: DateTime<Utc>,
    ) -> Result<InstrumentMapping, InstrumentResolverError> {
        let market_key = market.market_family.to_owned();
        let chain = self.provider.contract_chain(&market)?;

        if chain.is_empty() {
            return Err(InstrumentResolverError::EmptyContractChain { market: market_key });
        }

        validate_chain(&market_key, &chain)?;

        let (selected, resolution_basis) =
            select_front_month(&chain, resolved_at.date_naive(), &market_key)?;

        let canonical_symbol = format_contract_symbol(market.symbol_root, &selected.month);
        let resolved_contract = FuturesContract {
            market_family: market.market_family.to_owned(),
            display_name: market.display_name.to_owned(),
            venue: market.venue.to_owned(),
            symbol_root: market.symbol_root.to_owned(),
            month: selected.month,
            canonical_symbol: canonical_symbol.clone(),
        };

        let databento_symbols = vec![DatabentoInstrument {
            dataset: market.databento_dataset.to_owned(),
            symbol: selected.databento_symbol.clone(),
            symbology: DatabentoSymbology::RawSymbol,
        }];

        let summary = format!(
            "{display} resolved to {canonical} | Databento: {databento} | Tradovate: {tradovate} | basis: {basis}",
            display = market.display_name,
            canonical = canonical_symbol,
            databento = selected.databento_symbol,
            tradovate = selected.tradovate_symbol,
            basis = resolution_basis_label(resolution_basis),
        );

        Ok(InstrumentMapping {
            market_family: market.market_family.to_owned(),
            market_display_name: market.display_name.to_owned(),
            contract_mode,
            resolved_contract,
            databento_symbols,
            tradovate_symbol: selected.tradovate_symbol.clone(),
            resolution_basis,
            resolved_at,
            summary,
        })
    }
}

pub fn supported_markets() -> &'static [MarketDefinition] {
    &SUPPORTED_MARKETS
}

fn lookup_market_definition(market_family: &str) -> Option<MarketDefinition> {
    let normalized = normalize_market_key(market_family);
    SUPPORTED_MARKETS.iter().copied().find(|market| {
        normalize_market_key(market.market_family) == normalized
            || market
                .aliases
                .iter()
                .any(|alias| normalize_market_key(alias) == normalized)
    })
}

fn validate_chain(
    market_key: &str,
    chain: &[ContractListing],
) -> Result<(), InstrumentResolverError> {
    let mut months = BTreeSet::new();

    for listing in chain {
        if !(1..=12).contains(&listing.month.month) {
            return Err(InstrumentResolverError::InvalidContractMonth {
                market: market_key.to_owned(),
                month: listing.month.month,
            });
        }

        if !months.insert(listing.month) {
            return Err(InstrumentResolverError::DuplicateContractMonth {
                market: market_key.to_owned(),
            });
        }
    }

    Ok(())
}

fn select_front_month(
    chain: &[ContractListing],
    as_of: NaiveDate,
    market_key: &str,
) -> Result<(ContractListing, FrontMonthSelectionBasis), InstrumentResolverError> {
    let mut ordered = chain.to_vec();
    ordered.sort_by_key(|listing| listing.month);

    for listing in &ordered {
        match listing.rollover_cutoff() {
            Some((cutoff, _basis)) if as_of >= cutoff => continue,
            Some((_cutoff, basis)) => return Ok((listing.clone(), basis)),
            None => return Ok((listing.clone(), FrontMonthSelectionBasis::ChainOrder)),
        }
    }

    Err(InstrumentResolverError::NoActiveContract {
        market: market_key.to_owned(),
        as_of,
    })
}

fn resolution_basis_label(basis: FrontMonthSelectionBasis) -> &'static str {
    match basis {
        FrontMonthSelectionBasis::FirstNoticeDate => "first_notice_date",
        FrontMonthSelectionBasis::LastTradeDate => "last_trade_date",
        FrontMonthSelectionBasis::ChainOrder => "chain_order",
    }
}

fn normalize_market_key(market: &str) -> String {
    market
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '_' && *character != '-')
        .flat_map(|character| character.to_lowercase())
        .collect()
}

fn format_contract_symbol(symbol_root: &str, month: &ContractMonth) -> String {
    format!(
        "{root}{month_code}{year}",
        root = symbol_root,
        month_code = futures_month_code(month.month),
        year = month.year,
    )
}

fn futures_month_code(month: u8) -> char {
    match month {
        1 => 'F',
        2 => 'G',
        3 => 'H',
        4 => 'J',
        5 => 'K',
        6 => 'M',
        7 => 'N',
        8 => 'Q',
        9 => 'U',
        10 => 'V',
        11 => 'X',
        12 => 'Z',
        _ => '?',
    }
}

const SUPPORTED_MARKETS: [MarketDefinition; 7] = [
    MarketDefinition {
        market_family: "gold",
        display_name: "COMEX Gold",
        venue: "COMEX",
        symbol_root: "GC",
        tradovate_symbol_root: "GC",
        databento_dataset: "GLBX.MDP3",
        aliases: &["gc"],
    },
    MarketDefinition {
        market_family: "silver",
        display_name: "COMEX Silver",
        venue: "COMEX",
        symbol_root: "SI",
        tradovate_symbol_root: "SI",
        databento_dataset: "GLBX.MDP3",
        aliases: &["si"],
    },
    MarketDefinition {
        market_family: "crude_oil",
        display_name: "NYMEX WTI Crude Oil",
        venue: "NYMEX",
        symbol_root: "CL",
        tradovate_symbol_root: "CL",
        databento_dataset: "GLBX.MDP3",
        aliases: &["cl", "crude", "oil"],
    },
    MarketDefinition {
        market_family: "es",
        display_name: "CME E-mini S&P 500",
        venue: "CME",
        symbol_root: "ES",
        tradovate_symbol_root: "ES",
        databento_dataset: "GLBX.MDP3",
        aliases: &["sp500", "sp_500", "s&p500"],
    },
    MarketDefinition {
        market_family: "nq",
        display_name: "CME E-mini Nasdaq-100",
        venue: "CME",
        symbol_root: "NQ",
        tradovate_symbol_root: "NQ",
        databento_dataset: "GLBX.MDP3",
        aliases: &["nasdaq", "nasdaq100"],
    },
    MarketDefinition {
        market_family: "ym",
        display_name: "CBOT E-mini Dow",
        venue: "CBOT",
        symbol_root: "YM",
        tradovate_symbol_root: "YM",
        databento_dataset: "GLBX.MDP3",
        aliases: &["dow"],
    },
    MarketDefinition {
        market_family: "rty",
        display_name: "CME E-mini Russell 2000",
        venue: "CME",
        symbol_root: "RTY",
        tradovate_symbol_root: "RTY",
        databento_dataset: "GLBX.MDP3",
        aliases: &["russell", "russell2000"],
    },
];

#[derive(Clone, Debug, Default)]
pub struct StaticContractChainProvider {
    chains: HashMap<String, Vec<ContractListing>>,
}

impl StaticContractChainProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin_chains() -> Self {
        Self::with_reference_date(Utc::now().date_naive())
    }

    pub fn insert_chain(
        &mut self,
        market_family: &str,
        chain: Vec<ContractListing>,
    ) -> Option<Vec<ContractListing>> {
        self.chains
            .insert(normalize_market_key(market_family), chain)
    }

    fn with_reference_date(reference_date: NaiveDate) -> Self {
        let mut provider = Self::new();
        let year = reference_date.year();

        provider.insert_chain(
            "gold",
            build_contract_chain("GC", &[2, 4, 6, 8, 10, 12], year, 2, 28),
        );
        provider.insert_chain(
            "silver",
            build_contract_chain("SI", &[3, 5, 7, 9, 12], year, 2, 26),
        );
        provider.insert_chain(
            "crude_oil",
            build_contract_chain("CL", &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], year, 2, 20),
        );
        provider.insert_chain(
            "es",
            build_contract_chain("ES", &[3, 6, 9, 12], year, 2, 15),
        );
        provider.insert_chain(
            "nq",
            build_contract_chain("NQ", &[3, 6, 9, 12], year, 2, 15),
        );
        provider.insert_chain(
            "ym",
            build_contract_chain("YM", &[3, 6, 9, 12], year, 2, 15),
        );
        provider.insert_chain(
            "rty",
            build_contract_chain("RTY", &[3, 6, 9, 12], year, 2, 15),
        );

        provider
    }
}

fn build_contract_chain(
    symbol_root: &str,
    cycle_months: &[u8],
    start_year: i32,
    years_ahead: i32,
    rollover_day: u32,
) -> Vec<ContractListing> {
    let mut chain = Vec::new();

    for year in start_year..=(start_year + years_ahead) {
        for &month in cycle_months {
            let contract_month = ContractMonth { year, month };
            let symbol = format_contract_symbol(symbol_root, &contract_month);
            chain.push(
                ContractListing::new(contract_month, symbol.clone(), symbol)
                    .with_first_notice_date(previous_month_date(year, month, rollover_day)),
            );
        }
    }

    chain
}

fn previous_month_date(year: i32, month: u8, day: u32) -> NaiveDate {
    let (cutoff_year, cutoff_month) = if month == 1 {
        (year - 1, 12)
    } else {
        (year, u32::from(month - 1))
    };

    NaiveDate::from_ymd_opt(cutoff_year, cutoff_month, day)
        .expect("built-in contract cutoff dates should be valid")
}

impl ContractChainProvider for StaticContractChainProvider {
    fn contract_chain(
        &self,
        market: &MarketDefinition,
    ) -> Result<Vec<ContractListing>, ContractChainError> {
        self.chains
            .get(&normalize_market_key(market.market_family))
            .cloned()
            .ok_or_else(|| ContractChainError::Unavailable {
                market: market.market_family.to_owned(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv_bot_core_types::{
        BrokerPreference, BrokerPreferences, DashboardDisplay, DataRequirements, EntryOrderType,
        EntryRules, ExecutionSpec, ExitRules, FailsafeRules, FlattenRule, MarketConfig,
        MarketSelection, PartialTakeProfitRule, PositionSizing, PositionSizingMode, ReversalMode,
        RiskLimits, ScalingConfig, SessionMode, SessionRules, SignalCombinationMode,
        SignalConfirmation, StateBehavior, StrategyMetadata, Timeframe, TradeManagement,
        TradeWindow, WarmupRequirements,
    };

    #[derive(Clone, Copy, Debug)]
    struct FixedClock {
        now: DateTime<Utc>,
    }

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.now
        }
    }

    fn fixed_clock() -> FixedClock {
        FixedClock {
            now: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
        }
    }

    fn gold_chain() -> Vec<ContractListing> {
        vec![
            ContractListing::new(
                ContractMonth {
                    year: 2026,
                    month: 6,
                },
                "GCM2026",
                "GCM2026",
            )
            .with_first_notice_date(NaiveDate::from_ymd_opt(2026, 5, 28).expect("valid date")),
            ContractListing::new(
                ContractMonth {
                    year: 2026,
                    month: 8,
                },
                "GCQ2026",
                "GCQ2026",
            )
            .with_first_notice_date(NaiveDate::from_ymd_opt(2026, 7, 29).expect("valid date")),
        ]
    }

    fn compiled_strategy(market: &str) -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "test_strategy".to_owned(),
                name: "Test Strategy".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "resolver tests".to_owned(),
                tags: Vec::new(),
                source: None,
                notes: None,
            },
            market: MarketConfig {
                market: market.to_owned(),
                selection: MarketSelection {
                    contract_mode: ContractMode::FrontMonthAuto,
                },
            },
            session: SessionRules {
                mode: SessionMode::FixedWindow,
                timezone: "America/New_York".to_owned(),
                trade_window: Some(TradeWindow {
                    start: "08:30:00".to_owned(),
                    end: "11:30:00".to_owned(),
                }),
                no_new_entries_after: None,
                flatten_rule: Some(FlattenRule {
                    mode: tv_bot_core_types::FlattenRuleMode::ByTime,
                    time: Some("13:00:00".to_owned()),
                }),
                allowed_days: Vec::new(),
            },
            data_requirements: DataRequirements {
                feeds: Vec::new(),
                timeframes: vec![Timeframe::OneMinute],
                multi_timeframe: false,
                requires: None,
            },
            warmup: WarmupRequirements {
                bars_required: [(Timeframe::OneMinute, 10)].into_iter().collect(),
                ready_requires_all: true,
            },
            signal_confirmation: SignalConfirmation {
                mode: SignalCombinationMode::All,
                primary_conditions: vec!["trend".to_owned()],
                n_required: None,
                secondary_conditions: Vec::new(),
                score_threshold: None,
                regime_filter: None,
                sequence: Vec::new(),
            },
            entry_rules: EntryRules {
                long_enabled: true,
                short_enabled: true,
                entry_order_type: EntryOrderType::Market,
                entry_conditions: None,
                max_entry_distance_ticks: None,
                entry_timeout_seconds: None,
                allow_reentry_same_bar: None,
                entry_filters: None,
            },
            exit_rules: ExitRules {
                exit_on_opposite_signal: false,
                flatten_on_session_end: true,
                exit_conditions: Vec::new(),
                timeout_exit: None,
                max_hold_seconds: None,
                emergency_exit_rules: None,
            },
            position_sizing: PositionSizing {
                mode: PositionSizingMode::Fixed,
                contracts: Some(1),
                max_risk_usd: None,
                min_contracts: None,
                max_contracts: None,
                fallback_fixed_contracts: None,
                rounding_mode: None,
            },
            execution: ExecutionSpec {
                reversal_mode: ReversalMode::FlattenFirst,
                scaling: ScalingConfig {
                    allow_scale_in: false,
                    allow_scale_out: false,
                    max_legs: 1,
                },
                broker_preferences: BrokerPreferences {
                    stop_loss: BrokerPreference::BrokerRequired,
                    take_profit: BrokerPreference::BrokerRequired,
                    trailing_stop: BrokerPreference::BotAllowed,
                },
            },
            trade_management: TradeManagement {
                initial_stop_ticks: 10,
                take_profit_ticks: 20,
                break_even: None,
                trailing: None,
                partial_take_profit: Some(PartialTakeProfitRule {
                    enabled: false,
                    targets: Vec::new(),
                }),
                post_entry_rules: None,
                time_based_adjustments: None,
            },
            risk: RiskLimits {
                daily_loss: tv_bot_core_types::DailyLossLimit {
                    broker_side_required: true,
                    local_backup_enabled: true,
                },
                max_trades_per_day: 2,
                max_consecutive_losses: 2,
                max_open_positions: Some(1),
                max_unrealized_drawdown_usd: None,
                cooldown_after_daily_stop: None,
                max_notional_exposure: None,
            },
            failsafes: FailsafeRules {
                no_new_entries_on_data_degrade: true,
                pause_on_broker_sync_mismatch: true,
                pause_on_reconnect_until_reviewed: Some(true),
                kill_on_repeated_order_rejects: None,
                abnormal_spread_guard: None,
                clock_sanity_required: Some(true),
                storage_health_required: Some(true),
            },
            state_behavior: StateBehavior {
                cooldown_after_loss_s: 60,
                max_reentries_per_side: 1,
                regime_mode: None,
                memory_reset_rules: None,
                post_win_cooldown_s: None,
                failed_setup_decay: None,
                reentry_logic: None,
            },
            dashboard_display: DashboardDisplay {
                show: vec!["pnl".to_owned()],
                default_overlay: "entries_exits".to_owned(),
                debug_panels: Vec::new(),
                custom_labels: None,
                preferred_chart_timeframe: None,
            },
        }
    }

    #[test]
    fn builtin_contract_chains_resolve_gold_for_current_runtime_window() {
        let resolver = FrontMonthResolver::new(
            StaticContractChainProvider::with_reference_date(
                NaiveDate::from_ymd_opt(2026, 4, 10).expect("valid date"),
            ),
            fixed_clock(),
        );

        let mapping = resolver
            .resolve_for_strategy(&compiled_strategy("gold"))
            .expect("built-in chains should resolve gold");

        assert_eq!(mapping.tradovate_symbol, "GCM2026");
        assert_eq!(mapping.resolved_contract.canonical_symbol, "GCM2026");
    }

    #[test]
    fn resolves_front_month_for_strategy_market_family() {
        let mut provider = StaticContractChainProvider::new();
        provider.insert_chain("gold", gold_chain());
        let resolver = FrontMonthResolver::new(provider, fixed_clock());

        let mapping = resolver
            .resolve_for_strategy(&compiled_strategy("gold"))
            .expect("resolver should succeed");

        assert_eq!(mapping.market_family, "gold");
        assert_eq!(mapping.resolved_contract.canonical_symbol, "GCM2026");
        assert_eq!(mapping.databento_symbols[0].symbol, "GCM2026");
        assert_eq!(mapping.tradovate_symbol, "GCM2026");
        assert_eq!(
            mapping.resolution_basis,
            FrontMonthSelectionBasis::FirstNoticeDate
        );
    }

    #[test]
    fn alias_lookup_and_rollover_choose_next_contract_after_cutoff() {
        let mut provider = StaticContractChainProvider::new();
        provider.insert_chain(
            "gold",
            vec![
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 4,
                    },
                    "GCJ2026",
                    "GCJ2026",
                )
                .with_first_notice_date(NaiveDate::from_ymd_opt(2026, 4, 1).expect("valid date")),
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 6,
                    },
                    "GCM2026",
                    "GCM2026",
                )
                .with_first_notice_date(NaiveDate::from_ymd_opt(2026, 5, 28).expect("valid date")),
            ],
        );
        let resolver = FrontMonthResolver::new(provider, fixed_clock());

        let mapping = resolver
            .resolve_market("GC", ContractMode::FrontMonthAuto)
            .expect("alias lookup should succeed");

        assert_eq!(mapping.resolved_contract.canonical_symbol, "GCM2026");
    }

    #[test]
    fn unsupported_market_fails_cleanly() {
        let resolver = FrontMonthResolver::new(StaticContractChainProvider::new(), fixed_clock());

        let error = resolver
            .resolve_market("corn", ContractMode::FrontMonthAuto)
            .expect_err("unsupported market should fail");

        assert_eq!(
            error,
            InstrumentResolverError::UnsupportedMarket {
                market: "corn".to_owned(),
            }
        );
    }

    #[test]
    fn empty_contract_chain_fails() {
        let mut provider = StaticContractChainProvider::new();
        provider.insert_chain("gold", Vec::new());
        let resolver = FrontMonthResolver::new(provider, fixed_clock());

        let error = resolver
            .resolve_market("gold", ContractMode::FrontMonthAuto)
            .expect_err("empty chain should fail");

        assert_eq!(
            error,
            InstrumentResolverError::EmptyContractChain {
                market: "gold".to_owned(),
            }
        );
    }

    #[test]
    fn duplicate_contract_months_fail_validation() {
        let mut provider = StaticContractChainProvider::new();
        provider.insert_chain(
            "gold",
            vec![
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 6,
                    },
                    "GCM2026",
                    "GCM2026",
                ),
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 6,
                    },
                    "GCM2026_ALT",
                    "GCM2026_ALT",
                ),
            ],
        );
        let resolver = FrontMonthResolver::new(provider, fixed_clock());

        let error = resolver
            .resolve_market("gold", ContractMode::FrontMonthAuto)
            .expect_err("duplicate months should fail");

        assert_eq!(
            error,
            InstrumentResolverError::DuplicateContractMonth {
                market: "gold".to_owned(),
            }
        );
    }

    #[test]
    fn no_active_contract_after_all_cutoffs_fail() {
        let mut provider = StaticContractChainProvider::new();
        provider.insert_chain(
            "gold",
            vec![
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 2,
                    },
                    "GCG2026",
                    "GCG2026",
                )
                .with_first_notice_date(NaiveDate::from_ymd_opt(2026, 1, 20).expect("valid date")),
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 4,
                    },
                    "GCJ2026",
                    "GCJ2026",
                )
                .with_first_notice_date(NaiveDate::from_ymd_opt(2026, 2, 20).expect("valid date")),
            ],
        );
        let resolver = FrontMonthResolver::new(provider, fixed_clock());

        let error = resolver
            .resolve_market("gold", ContractMode::FrontMonthAuto)
            .expect_err("no active contract should fail");

        assert_eq!(
            error,
            InstrumentResolverError::NoActiveContract {
                market: "gold".to_owned(),
                as_of: NaiveDate::from_ymd_opt(2026, 4, 10).expect("valid date"),
            }
        );
    }

    #[test]
    fn chain_order_fallback_is_used_when_cutoff_dates_are_missing() {
        let mut provider = StaticContractChainProvider::new();
        provider.insert_chain(
            "es",
            vec![
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 6,
                    },
                    "ESM2026",
                    "ESM2026",
                ),
                ContractListing::new(
                    ContractMonth {
                        year: 2026,
                        month: 9,
                    },
                    "ESU2026",
                    "ESU2026",
                ),
            ],
        );
        let resolver = FrontMonthResolver::new(provider, fixed_clock());

        let mapping = resolver
            .resolve_market("es", ContractMode::FrontMonthAuto)
            .expect("resolver should succeed");

        assert_eq!(mapping.resolved_contract.canonical_symbol, "ESM2026");
        assert_eq!(
            mapping.resolution_basis,
            FrontMonthSelectionBasis::ChainOrder
        );
    }
}
