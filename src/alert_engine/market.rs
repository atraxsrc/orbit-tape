//! Market data for the alert engine: symbol details, closed candles and
//! live trades, from the exchanges or, in tests, from files.

use exchange::adapter::{AdapterHandles, Exchange, MarketKind, StreamConfig, Venue};
use exchange::{Event, Kline, PushFrequency, Ticker, TickerInfo, Timeframe};
use iced::futures::stream::BoxStream;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait MarketData: Send + Sync + 'static {
    /// Tick size and the other details of a listed instrument.
    fn ticker_info(&self, ticker: Ticker) -> BoxFuture<'_, Result<TickerInfo, String>>;

    /// Candles with open times in `[from, to)`, oldest first.
    fn klines(
        &self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        from: u64,
        to: u64,
    ) -> BoxFuture<'_, Result<Vec<Kline>, String>>;

    /// Live trades of `tickers` on one exchange. Reconnects by itself and
    /// reports `Connected` / `Disconnected` along the way.
    fn trades(&self, exchange: Exchange, tickers: Vec<TickerInfo>) -> BoxStream<'static, Event>;
}

type Listing = HashMap<Ticker, Option<TickerInfo>>;

/// The exchanges, over the same adapters the charts use.
pub struct LiveMarkets {
    handles: AdapterHandles,
    listings: Mutex<HashMap<(Venue, MarketKind), Listing>>,
}

impl LiveMarkets {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| format!("could not build the HTTP client: {error}"))?;
        Ok(Self {
            handles: AdapterHandles::spawn_venues(&client, Venue::ALL, None),
            listings: Mutex::new(HashMap::new()),
        })
    }

    fn cached(&self, key: (Venue, MarketKind), ticker: &Ticker) -> Option<Option<TickerInfo>> {
        let listings = self
            .listings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        listings
            .get(&key)
            .map(|listing| listing.get(ticker).copied().flatten())
    }
}

fn not_listed(ticker: &Ticker) -> String {
    format!(
        "{} is not listed on {} ({})",
        ticker.display_symbol_and_type().0,
        ticker.exchange.venue(),
        ticker.exchange.market_type()
    )
}

impl MarketData for LiveMarkets {
    fn ticker_info(&self, ticker: Ticker) -> BoxFuture<'_, Result<TickerInfo, String>> {
        Box::pin(async move {
            let key = (ticker.exchange.venue(), ticker.exchange.market_type());
            if let Some(found) = self.cached(key, &ticker) {
                return found.ok_or_else(|| not_listed(&ticker));
            }
            let listing = self
                .handles
                .fetch_ticker_metadata(key.0, &[key.1])
                .await
                .map_err(|error| format!("could not load the {} symbols: {error}", key.0))?;
            let found = listing.get(&ticker).copied().flatten();
            self.listings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key, listing);
            found.ok_or_else(|| not_listed(&ticker))
        })
    }

    fn klines(
        &self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        from: u64,
        to: u64,
    ) -> BoxFuture<'_, Result<Vec<Kline>, String>> {
        Box::pin(crate::headless::fetch_kline_range(
            &self.handles,
            ticker_info,
            timeframe,
            from,
            to,
        ))
    }

    fn trades(&self, exchange: Exchange, tickers: Vec<TickerInfo>) -> BoxStream<'static, Event> {
        self.handles.trade_stream(&StreamConfig::new(
            tickers,
            exchange,
            None,
            PushFrequency::ServerDefault,
        ))
    }
}
