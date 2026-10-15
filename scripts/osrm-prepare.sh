#!/usr/bin/env sh
# Fetches the roads of Kyiv from OpenStreetMap and prepares them for the OSRM server of
# the `osrm` profile, which gives the solvers distances by road (D67).
#
#   scripts/osrm-prepare.sh
#
# Run it once, and again whenever newer roads are wanted. It needs Docker, curl and the
# network: for the extract, some 35 MB, and the first time for the image of OSRM, some
# 400 MB. What it makes is in osrm/, which git ignores.
#
# After it, in .env:
#
#   COMPOSE_PROFILES=osrm
#   OSRM_URL=http://osrm:5000
#
# and `docker compose up --build --scale solver=4` brings the stack up with an OSRM
# server in it, and solvers that ask it.
#
# The extract is BBBike's of Kyiv: the rectangle from 30.28 to 30.86 degrees east and
# from 50.30 to 50.62 degrees north, cut out of OpenStreetMap once a week. An order
# outside it is taken for one at the nearest road inside. OSRM_EXTRACT takes another
# address: any .osm.pbf that holds the depot and what is ordered round it.
#
# The data is (c) OpenStreetMap contributors, under the Open Database License:
# https://www.openstreetmap.org/copyright
set -eu

extract=${OSRM_EXTRACT:-https://download.bbbike.org/osm/bbbike/Kiew/Kiew.osm.pbf}

cd "$(dirname "$0")/.."
mkdir -p osrm

# A server that is running reads the files this is about to write.
docker compose --profile osrm stop osrm >/dev/null 2>&1 || true

echo "Fetching $extract"
# Under another name until it is whole, so that half a download is never taken for the
# roads.
curl --fail --location --silent --show-error --output osrm/kyiv.osm.pbf.part "$extract"
rm -f osrm/kyiv.osrm*
mv -f osrm/kyiv.osm.pbf.part osrm/kyiv.osm.pbf
megabytes=$(($(wc -c <osrm/kyiv.osm.pbf) / 1048576))
echo "Got $megabytes MB. Preparing the roads for cars, which takes a minute or two."

# The roads a car may drive, and then the shortcuts that make a table quick.
docker compose run --rm osrm-tools osrm-extract -p /opt/car.lua /data/kyiv.osm.pbf
docker compose run --rm osrm-tools osrm-contract /data/kyiv.osrm

megabytes=$(($(du -sk osrm | cut -f1) / 1024))
echo
echo "The roads are in osrm/, $megabytes MB. To use them, put into .env:"
echo '  COMPOSE_PROFILES=osrm'
echo '  OSRM_URL=http://osrm:5000'
echo 'and start the stack: docker compose up --build --scale solver=4'
