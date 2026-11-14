# Next ▸ hand-off

Use list panes as a workflow: select a name, press **N**, and arrive in the next
view with that canonical security already filled in. Enter retains each pane's
existing chart action.

| Key / control | Action |
| --- | --- |
| N | Open the selected source's default next view |
| Shift+N | Open its alternate destination picker |
| ↑/↓, Enter in the picker | Choose and confirm an alternate |
| Esc / click outside the picker | Close without creating a hop |
| Backspace / `← from …` crumb | Return one hop, restoring the layout and source selection |
| ▸ beside a live crumb | Open alternates for the current selected source |
| Delete on a chart | Delete its selected drawing, including while a hop is live |
| Backspace on a chart with no hops | Delete its selected drawing, as before |

N is a pane key. The command **N remains NEWS**. No NEXT or BACK commands were
added; explicit securities such as `NEXT US Equity` and `BACK US Equity` still
load as tickers. Hand-off keys require an empty command line and no focused text
editor or command bar. Ctrl, Command and Alt chords do not trigger N. With no
source selection, N remains available to type into the command bar. The archive
management/history view is not a hand-off source.

| Selected source | N | Shift+N / ▸ alternates |
| --- | --- | --- |
| SCAN / UNIV | Archive TWALL D; select the same symbol | CLOCK, chart + TSTACK |
| RRG | Intraday TWALL of its list; select the same symbol | COCKPIT, CLOCK, chart + TSTACK |
| Intraday TWALL | COCKPIT: linked native 1m / 5m / 15m charts, TSTACK and RAIL | CLOCK, Auction Map on a 5m chart, BRIEF |
| Archive TWALL | CLOCK, default 30m buckets | Auction Map on a daily chart, TSTACK TUNE daily, BRIEF |
| CLOCK cell or selected symbol heading | 5m chart with Auction Map enabled | SPATH, BRIEF, COCKPIT |

SCAN and UNIV currently supply dated daily research, so their wall uses D/W/M
archive mode. A visible filtered list of 2–24 names becomes the wall; other sizes
produce a one-name wall. RRG passes its watchlist or explicit symbol list without
its benchmark, cadence or replay settings. If the selected RRG name falls beyond
TWALL’s 24-row cap, it moves to the front; the remaining capped names stay listed
as not loaded. Its archive chart alternate retains
Enter's archive mode; scanner chart alternates enable TSTACK.

In CLOCK, click a historical cell (including a hole or LOW N cell), or use arrows
to select and move. Arrows stop at grid edges. TODAY is descriptive and is not a
selectable historical cell. Clicking the **Session Clock** heading selects the
symbol without a bucket; arrival from another pane also selects the symbol.
A cell hand-off names its weekday and bucket in status. It does not move a chart
to an inferred session or manufacture any missing observations.

The lower status bar shows a clickable `← from TWALL · AAPL` crumb. A depth suffix
appears after the first hop, and `⌫ back` is shown beside it. The stack holds up to
eight snapshots and drops the oldest when full. It lives only in memory: it is
not in layouts or preferences, and restarting clears it. Layout snapshots include
pane settings, links, titles, focus and pop-out geometry. The departing source's
cached data and transient state also return: scanner filters/selection, wall
sort/selection, RRG replay frame/play state, and CLOCK selection. Pane ids are
renewed so old worker responses cannot reach restored panes.

A COCKPIT hand-off replaces the workspace temporarily. Unlike running the COCKPIT
command, it does not save either the departing named layout or Cockpit. Automatic
layout/settings saving pauses while hops are live; explicit save commands remain
available. Backspace restores the actual pre-hop workspace, including unsaved
changes and its prior saved/dirty state, without switching through a named save.
As with other full-layout restores, destination changes made after departure are
not carried back. A source closed after departure still returns from its snapshot.

Arrival outlines the wall's selected symbol row and shows an accent symbol chip
on destinations for about 0.6 seconds. Reduce Motion keeps the outline/chip static
until the next click, text input or key. The picker and crumb use the terminal's
current theme.

Hand-off uses existing worker requests and verified-data caches. It adds no worker
endpoint, network service, disk read or synchronous save. Sample/offline display
mode refuses the hop with the existing **NO REAL DATA** card and creates no stack
entry. Destination panes retain their existing real-data admission/refusal rules.

This slice does not add ISCAN hand-off, chart-source hand-off, a CLOCK session lens
jump, persisted navigation, journals, trade plans or alerts. Keyboard hand-off is
limited to sources docked in the main window. Other pop-out windows survive return;
if an existing destination is popped out, its replacement is docked for the hop
and the original window is restored on return. Native-window interaction has not
been exercised by launching the GUI in this change.
