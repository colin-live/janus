"""World-frame conversion to the protocol's canonical ENU (right-handed, Z up) and back.

Webots ``WorldInfo.coordinateSystem``: ``ENU`` (identity) and ``NUE`` (x=North, y=Up, z=East) are
right-handed and supported. ``EUN`` is left-handed and rejected in v1.
"""

from __future__ import annotations

import math
from typing import Sequence, Tuple

SUPPORTED = ("ENU", "NUE")

Vec3 = Tuple[float, float, float]
Quat = Tuple[float, float, float, float]

# NUE -> ENU:  E = z, N = x, U = y
_P_NUE_TO_ENU = ((0.0, 0.0, 1.0), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0))


def check_supported(cs: str) -> None:
    if cs not in SUPPORTED:
        raise ValueError("coordinateSystem %r is not supported (supported: %s)" % (cs, ", ".join(SUPPORTED)))


def vec_to_enu(v: Sequence[float], cs: str) -> Vec3:
    if cs == "ENU":
        return (v[0], v[1], v[2])
    if cs == "NUE":
        return (v[2], v[0], v[1])
    raise ValueError(cs)


def vec_from_enu(v: Sequence[float], cs: str) -> Vec3:
    if cs == "ENU":
        return (v[0], v[1], v[2])
    if cs == "NUE":
        return (v[1], v[2], v[0])
    raise ValueError(cs)


def matrix_to_enu(m: Sequence[float], cs: str) -> Tuple[float, ...]:
    """Row-major 3x3 rotation (as returned by ``Node.getOrientation``) expressed in the ENU world."""
    if cs == "ENU":
        return tuple(m)
    if cs != "NUE":
        raise ValueError(cs)
    P = _P_NUE_TO_ENU
    R = [[m[3 * i + j] for j in range(3)] for i in range(3)]
    # R' = P R P^T
    PR = [[sum(P[i][k] * R[k][j] for k in range(3)) for j in range(3)] for i in range(3)]
    out = [[sum(PR[i][k] * P[j][k] for k in range(3)) for j in range(3)] for i in range(3)]
    return tuple(out[i][j] for i in range(3) for j in range(3))


def matrix_to_quat(m: Sequence[float]) -> Quat:
    """Rotation matrix (row-major) -> quaternion (x, y, z, w), w >= 0."""
    r00, r01, r02, r10, r11, r12, r20, r21, r22 = m
    tr = r00 + r11 + r22
    if tr > 0.0:
        s = math.sqrt(tr + 1.0) * 2.0
        w, x, y, z = 0.25 * s, (r21 - r12) / s, (r02 - r20) / s, (r10 - r01) / s
    elif r00 > r11 and r00 > r22:
        s = math.sqrt(1.0 + r00 - r11 - r22) * 2.0
        w, x, y, z = (r21 - r12) / s, 0.25 * s, (r01 + r10) / s, (r02 + r20) / s
    elif r11 > r22:
        s = math.sqrt(1.0 + r11 - r00 - r22) * 2.0
        w, x, y, z = (r02 - r20) / s, (r01 + r10) / s, 0.25 * s, (r12 + r21) / s
    else:
        s = math.sqrt(1.0 + r22 - r00 - r11) * 2.0
        w, x, y, z = (r10 - r01) / s, (r02 + r20) / s, (r12 + r21) / s, 0.25 * s
    if w < 0.0:
        w, x, y, z = -w, -x, -y, -z
    return (x, y, z, w)
