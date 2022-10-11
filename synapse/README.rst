Matrix Synapse module which implements MSC3886 Simple HTTP rendezvous server
============================================================================

This is an implementation of `MSC3886
<https://github.com/matrix-org/matrix-spec-proposals/pull/3886>`_ for `Synapse
<https://github.com/matrix-org/synapse>`_.

=====
Usage
=====

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
