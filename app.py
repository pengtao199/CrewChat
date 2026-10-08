"""Chainlit entrypoint: `chainlit run app.py`"""

import crewchat.auth  # noqa: F401  (registers @cl.password_auth_callback)
import crewchat.chat  # noqa: F401  (registers the @cl.* handlers)
