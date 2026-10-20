"""Small numeric helpers shared by the analysis modules."""

from __future__ import annotations

import numpy as np
import pandas as pd


def pcts(values, ps=(50, 99), with_max: bool = False, nd: int = 1) -> dict | None:
    """``{"p50": .., "p99": .., ["max": ..]}`` of the non-null values, rounded
    to ``nd`` decimals; None when there are none."""
    v = pd.to_numeric(pd.Series(values), errors="coerce").dropna()
    if v.empty:
        return None
    out = {f"p{p}": round(float(np.percentile(v, p)), nd) for p in ps}
    if with_max:
        out["max"] = round(float(v.max()), nd)
    return out
