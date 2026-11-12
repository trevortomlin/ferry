#!/bin/sh
# Run by atmoz/sftp before sshd starts.
#
# The test suite deliberately tries a wrong password. OpenSSH 9.8+ answers that
# by penalising the source IP for ~18 seconds, and it penalises bare TCP probes
# too, which made every test that ran afterwards fail with "Disconnected". That
# behaviour is correct for a real server and wrong for a fixture.
cat >> /etc/ssh/sshd_config <<'CONF'
PerSourcePenalties no
MaxStartups 100:30:200
CONF
