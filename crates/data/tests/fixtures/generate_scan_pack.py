"""Run from wn60 with PYTHONDONTWRITEBYTECODE=1; only output is the supplied path.
RSI only: python3 /path/to/this/script.py --rsi /path/to/scan_pack_rsi.json
PYTHONDONTWRITEBYTECODE=1 python3 /path/to/this/script.py /path/to/scan_pack.json
"""
import json
import sys
from unittest.mock import patch
import numpy as np
import pandas as pd
# The read-only reference is the current working directory, not this script's dir.
sys.path.insert(0, '.')
import scan_pack as sp

# Qulla-only mode writes a separate compact fixture, never existing fixtures.
if len(sys.argv) > 1 and sys.argv[1] == '--qulla':
    def compact(values):
        runs = []
        for v in values:
            v = 'nan' if np.isnan(v) else float(v)
            if runs and runs[-1][1] == v: runs[-1][0] += 1
            else: runs.append([1, v])
        return runs
    output = []
    for name in ['miss', 'exact_4', 'rounded_4', 'below_4', 'vol_only', 'near_only', 'zero_open', 'nan_open', 'nan_prev', 'zero_prev', 'missing_open', 'negative_open_prev']:
        close, high, volume, opn = [np.full(30, v) for v in [90., 100., 100., 90.]]
        if name in ['exact_4', 'rounded_4', 'below_4']:
            opn[-1] = {'exact_4':93.6, 'rounded_4':93.599, 'below_4':93.59}[name]
            volume[-1] = 150.
        if name == 'vol_only': volume[-1] = 150.
        if name == 'near_only': close[-1] = 96.
        if name == 'zero_open': opn[-1] = 0.
        if name == 'nan_open': opn[-1] = np.nan
        if name == 'nan_prev': close[-2] = np.nan
        if name == 'zero_prev': close[-2] = 0.
        if name == 'negative_open_prev': close[-2], opn[-1] = -90., -93.6
        frame = dict(close=close, high=high, volume=volume)
        if name != 'missing_open': frame['open'] = opn
        row = sp.measure(name, df=pd.DataFrame(frame))
        output.append(dict(name=name, close=compact(close), high=compact(high), volume=compact(volume),
                           open=compact(opn) if 'open' in frame else None,
                           expected=dict(gap_pct=row['gap_pct'], is_ep='EP' in row['tags'], qulla=row['match']['qulla'])))
    with open(sys.argv[2], 'w') as out:
        json.dump(output, out, separators=(',', ':'), allow_nan=False)
    sys.exit(0)

# Breakout-only mode leaves the existing fixtures untouched. Compact constant
# runs keep the 252-bar window cases small while preserving NaN positions.
if len(sys.argv) > 1 and sys.argv[1] == '--breakout':
    keys = 'dist_52w_pct dist_nd_pct near_52w near_nd vol_ratio vol_surge'.split()
    def run(n, close=95., high=100., volume=100.):
        return [np.full(n, close), np.full(n, high), np.full(n, volume)]
    cases = {name: run(n) for name, n in [('empty',0),('two',2),('short_high',9),('minimum_nd',10),('before_52w',59),('minimum_52w',60),('window_252',260)]}
    cases['exact_threshold'] = run(60); cases['exact_threshold'][2][-1] = 150.
    cases['rounded_inside'] = run(60, close=94.996); cases['rounded_inside'][2][-1] = 149.6
    cases['outside'] = run(60, close=94.994); cases['outside'][2][-1] = 149.4
    cases['nan_high'] = run(60); cases['nan_high'][1][-1] = np.nan
    cases['nan_volume'] = run(60); cases['nan_volume'][2][-1] = np.nan
    cases['nan_prior_volume'] = run(60); cases['nan_prior_volume'][2][-3:] = [np.nan, np.nan, 150.]
    cases['nan_close'] = run(60); cases['nan_close'][0][-1] = np.nan
    cases['window_252'][1][0] = 1000.
    def compact(values):
        runs = []
        for v in values:
            v = 'nan' if np.isnan(v) else float(v)
            if runs and runs[-1][1] == v: runs[-1][0] += 1
            else: runs.append([1,v])
        return runs
    output = []
    for name, (close, high, volume) in cases.items():
        if len(close):
            row = sp.measure(name, df=pd.DataFrame(dict(close=close,high=high,volume=volume)))
            expected = {k: row[k] for k in keys}; match = row['match']['breakout']
        else:
            expected = dict(dist_52w_pct=None,dist_nd_pct=None,near_52w=False,near_nd=False,vol_ratio=None,vol_surge=False); match = False
        output.append(dict(name=name,close=compact(close),high=compact(high),volume=compact(volume),expected=expected,match=match))
    with open(sys.argv[2], 'w') as out:
        json.dump(output,out,separators=(',', ':'),allow_nan=False)
    sys.exit(0)

# RSI-only mode never opens or regenerates the original MA/breadth fixture.
if len(sys.argv) > 1 and sys.argv[1] == '--rsi':
    rkeys = 'rsi14 rsi14_prev rsi_os rsi_ob rsi_rising_from_os'.split()
    rcases = {
        'up': np.linspace(80, 120, 32),
        'down': np.linspace(120, 80, 32),
        'flat': np.full(32, 100.),
        'empty': np.array([]),
        'short': np.linspace(80, 120, 15),
        'minimum': np.r_[np.linspace(120, 80, 15), 81.],
        'oversold_exit': np.r_[np.linspace(120, 80, 30), 81., 110.],
        'one_rsi_value': np.r_[np.linspace(120, 100, 8), np.nan, np.linspace(101, 105, 8)],
        'oversold_rising': np.r_[np.linspace(120, 80, 30), 82., 84.],
        'mixed': np.array([100 + i*.1 + (-1)**i*2 for i in range(40)]),
    }
    for name, positions in [('nan_last', [-1]), ('nan_prev', [-2]), ('nan_gap', [18,19,20])]:
        x = rcases['mixed'].copy()
        x[positions] = np.nan
        rcases[name] = x
    output = []
    for name, close in rcases.items():
        if len(close):
            row = sp.measure(name, df=pd.DataFrame({'close': close}))
            expected = {k: row[k] for k in rkeys}
            match = row['match']['rsi']
        else:
            expected = dict(rsi14=None, rsi14_prev=None, rsi_os=False, rsi_ob=False, rsi_rising_from_os=False)
            match = False
        output.append(dict(name=name, close=['nan' if np.isnan(v) else float(v) for v in close], expected=expected, match=match))
    with open(sys.argv[2], 'w') as out:
        json.dump(output, out, separators=(',', ':'), allow_nan=False)
    sys.exit(0)

keys = 'ready price day_pct bars sma20 sma50 sma200 sma20_rising sma50_rising sma200_rising vs20 vs50 vs200 stacked_ma pullback_rising_ma'.split()
bkeys = 'ready n stored_n n_sma50 n_sma200 n_ad_1d n_ad_5d pct_above_sma50 pct_above_sma200 adv_1d dec_1d unch_1d adv_5d dec_5d unch_5d ad_1d ad_5d'.split()
cases = {}
for n in [0,1,5,6,19,20,21,49,50,51,199,200,201,220]:
    cases[f'up_{n}'] = np.linspace(80,150,n)
cases['down'] = np.linspace(150,80,220)
cases['flat'] = np.full(220,100.)
x = np.linspace(100,140,80); x[-1] = x[-20:].mean()*.99
cases['pullback20'] = x
# Rising SMA50 band with SMA20 far below the current price.
cases['pullback50'] = np.r_[np.full(160,70.),np.full(30,120.),np.full(19,90.),106.]
for name, index in [('nan_last',-1),('nan_previous',-2),('nan_five',-6),('nan_20',-20),('nan_50',-50),('nan_200',-200),('nan_outside',0)]:
    x = np.linspace(80,150,220); x[index]=np.nan; cases[name]=x
cases['zero'] = np.zeros(220)
cases['negative'] = -np.ones(220)
cases['zero_previous'] = np.r_[np.full(218,100.),0.,100.]
cases['rounded_unchanged'] = np.r_[np.full(219,100.),100.00001]
cases['pos_inf'] = np.r_[np.full(219,100.),np.inf]
cases['neg_inf'] = np.r_[np.full(219,100.),-np.inf]
# Rounded vs is used by the inclusive band, including just outside raw bounds.
for target in [-3.006,-3.004,-3.,.5,.504,.506]:
    x = np.linspace(100,140,80)
    ratio=1+target/100
    x[-1]=ratio*x[-20:-1].sum()/(20-ratio)
    cases[f'band_{target}']=x
frames={k:pd.DataFrame({'close':v, 'high':np.full(len(v),200.), 'low':np.full(len(v),1.)}) for k,v in cases.items() if len(v)}
rows={k:sp.measure(k,df=f) for k,f in frames.items()}
# Empty measure has a sparse payload; normalize to the pure Rust default.
def expected(k):
    if not len(cases[k]):
        return dict(ready=False,price=None,day_pct=None,bars=0,**{v:False if v in ['stacked_ma','pullback_rising_ma'] else None for v in keys[4:]})
    return {key:rows[k][key] for key in keys}
def encode(x):
    return 'nan' if np.isnan(x) else 'inf' if x==np.inf else '-inf' if x==-np.inf else float(x)
groups=[[], ['flat'], ['up_1'], ['nan_last'], ['up_220','down','flat'],list(frames), ['up_5','up_6','nan_five','zero_previous','rounded_unchanged']]
with patch.object(sp,'_stored_bar_count',return_value=0), patch.object(sp.md,'list_desk_symbols',return_value=[]):
    breadth=[{'names':g,'expected':{k:v for k,v in sp._breadth_from_rows([rows[n] for n in g],{k.upper():v for k,v in frames.items()}).items() if k in bkeys}} for g in groups]
with open(sys.argv[1],'w') as out:
    json.dump({'cases':[{'name':k,'close':[encode(v) for v in x],'expected':expected(k)} for k,x in cases.items()], 'breadth':breadth},out,indent=2,allow_nan=False)
