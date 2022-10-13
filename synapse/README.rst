Matrix Synapse module which implements MSC3886 Simple HTTP rendezvous server
============================================================================

This is an implementation of `MSC3886
<https://github.com/matrix-org/matrix-spec-proposals/pull/3886>`_ for `Synapse
<https://github.com/matrix-org/synapse>`_.

-----
Usage
-----

1. Install the module to make it available to your Synapse environment::

    pip install matrix-http-rendezvous-synapse

2. Enable the module in your homeserver.yaml::

    modules:
      - module: matrix_http_rendezvous_synapse.SynapseRendezvousModule
        config:
          prefix: /_synapse/client/org.matrix.msc3886/rendezvous

3. Make the module available at the actual API endpoint in the Client-Server API by adding this to your homeserver.yaml::

    experimental_features:
      msc3886_endpoint: /_synapse/client/org.matrix.msc3886/rendezvous

4. Run Synapse with the asyncio reactor enabled::

    SYNAPSE_ASYNC_IO_REACTOR=1 python -m synapse.app.homeserver

---------------------
Configuration options
---------------------

Apart from the `prefix` the following config options are available:

- `ttl`: The time-to-live of the rendezvous session in seconds. Defaults to 60.
- `max_bytes`: The maximum number of bytes that can be sent in a single request. Defaults to 4096.

An example configuration setting these and a custom prefix would like::

    modules:
      - module: matrix_http_rendezvous_synapse.SynapseRendezvousModule
        config:
          prefix: /rendezvous
          ttl: 15                   # seconds
          max_bytes: 10240          # 10 KiB

    experimental_features:
      msc3886_endpoint: /rendezvous # this should match above
