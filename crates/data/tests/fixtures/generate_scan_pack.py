"""Run from wn60 with PYTHONDONTWRITEBYTECODE=1; only output is the supplied path.
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
