"""Janus Webots adapter (Block B8): exchanges per-step state/effort data with external model processes
over shared memory. See docs/blocks/B8-webots-py.md and docs/01-protocol-v1.md."""

from .adapter import CouplingAdapter
from .channel import CouplingError

__all__ = ["CouplingAdapter", "CouplingError"]
__version__ = "0.1.0"
