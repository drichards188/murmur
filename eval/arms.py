"""Arms: who proposes and who decides.

| arm                 | proposer                          | supervisor                      |
|---------------------|-----------------------------------|---------------------------------|
| rules-only          | scripted agent (MCP)              | auto-approve (approve all)      |
| rules-only-reject   | scripted agent (MCP)              | auto-reject (reject all)        |
| human-proxy         | a second human operator (REST)    | proxy operator (`eval/proxy.py`)|
| agent+human-proxy   | an agent's own proposals (MCP)    | proxy operator                  |
| agent+model         | a real model via MCP (optional)   | proxy operator                  |
| study               | scripted agent (MCP)              | a real participant, console     |

The same scenario (fleet, contacts, events, policy, seeds) drives every arm. In the
storyline arms the storyline's proposals go through the proposer named; in agent+human-proxy
the agent makes its own proposals from what it reads (eval/agent.py `AgentPolicy`), so its
proposal stream differs from the storyline's and is labelled by truth the same way.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Arm:
    name: str
    proposer: str
    supervisor: str
    description: str
    web: bool = False
    external_model: bool = False


ARMS: dict[str, Arm] = {
    a.name: a
    for a in (
        Arm(
            "rules-only",
            "agent",
            "auto-approve",
            "policy auto-tiers plus a script that approves everything that reaches the human tier",
        ),
        Arm(
            "rules-only-reject",
            "agent",
            "auto-reject",
            "policy auto-tiers plus a script that rejects everything that reaches the human tier",
        ),
        Arm(
            "human-proxy",
            "human",
            "proxy",
            "human-only: a second operator proposes, the proxy operator decides in the inbox",
        ),
        Arm(
            "agent+human-proxy",
            "agent-autonomous",
            "proxy",
            "an agent proposes on its own from the world model it reads over MCP (eval/agent.py AgentPolicy), the proxy operator decides",
        ),
        Arm(
            "agent+model",
            "model",
            "proxy",
            "a real model proposes through the MCP server (Anthropic API, cost-guarded), the proxy decides",
            external_model=True,
        ),
        Arm(
            "study",
            "agent",
            "participant",
            "a real participant decides in the console; the runner only records",
            web=True,
        ),
    )
}


def arm(name: str) -> Arm:
    if name not in ARMS:
        raise ValueError(f"unknown arm {name!r}; arms: {', '.join(ARMS)}")
    return ARMS[name]
