# Watchlist news and chart catalysts

`NEWS` (aliases `N`, `CN`) opens **watchlist headlines**. `AAPL CN` is still one
name. `TOP` is one general-market query. The pane never polls the archive
universe.

Yahoo supplies equity / ETF / index / FX headlines; CoinDesk supplies crypto.
Publication times come from the feed (`providerPublishTime` / RSS `pubDate`).
`TR_PROVIDER=sample` returns no headlines and invents none.

## Scope

| Control | Names fetched | Cap |
|---|---|---|
| Watchlist | Selected watchlist group | 12 |
| Leaders | Top rows of an open SCAN pane | 12 |
| This name / `AAPL CN` | That market key | 1 |
| Top News / `TOP` | One `"stock market"` query | 1 query |

Scanner leaders are the already-ranked SCAN rows. The archive is not re-read
for news.

## Freshness and failure

Fetches are cached for 5 minutes (32 names). After 15 minutes the pane shows
**Stale**. If some names fail, successful headlines stay and the failed names
are listed. A refresh that fails keeps the last good list and marks it stale.

## Chart markers

Visible charts load news for **their own symbol** only. When a headline's
timestamp falls inside a loaded bar, an amber **N** pin is drawn at the top of
the plot (same idea as Whats-News `news_markers.js`). Hover shows title,
publisher and time; click opens the story. The overlay is saved with the
layout (`show_news`, default on). Sample / unverified prices still never enter
research; sample mode draws no invented pins.

## Isolation

Sample data is not substituted into news. Cached headlines are not written to
the price archive. Step 9 price/setup alerts and Step 10 journal are out of
scope.
