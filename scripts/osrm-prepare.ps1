# Fetches the roads of Kyiv from OpenStreetMap and prepares them for the OSRM server of
# the `osrm` profile, which gives the solvers distances by road (D67).
#
#   scripts/osrm-prepare.ps1
#
# Run it once, and again whenever newer roads are wanted. It needs Docker and the
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
# outside it is taken for one at the nearest road inside. -Extract takes another
# address: any .osm.pbf that holds the depot and what is ordered round it.
#
# The data is (c) OpenStreetMap contributors, under the Open Database License:
# https://www.openstreetmap.org/copyright
param(
    [string]$Extract = 'https://download.bbbike.org/osm/bbbike/Kiew/Kiew.osm.pbf'
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell draws a progress bar for a download, and is slow at it.
$ProgressPreference = 'SilentlyContinue'

function Compose {
    docker compose @args
    if ($LASTEXITCODE -ne 0) { throw "docker compose $args failed" }
}

Push-Location (Join-Path $PSScriptRoot '..')
try {
    New-Item -ItemType Directory -Force osrm | Out-Null

    # A server that is running reads the files this is about to write. Where there is
    # none, compose says so on standard error, which is no reason to stop here.
    $ErrorActionPreference = 'Continue'
    docker compose --profile osrm stop osrm 2>&1 | Out-Null
    $ErrorActionPreference = 'Stop'

    Write-Host "Fetching $Extract"
    # Under another name until it is whole, so that half a download is never taken for
    # the roads.
    Invoke-WebRequest -Uri $Extract -OutFile osrm/kyiv.osm.pbf.part -UseBasicParsing
    Get-ChildItem osrm -Filter 'kyiv.osrm*' | Remove-Item -Force
    Move-Item -Force osrm/kyiv.osm.pbf.part osrm/kyiv.osm.pbf
    $megabytes = [math]::Round((Get-Item osrm/kyiv.osm.pbf).Length / 1MB)
    Write-Host "Got $megabytes MB. Preparing the roads for cars, which takes a minute or two."

    # The roads a car may drive, and then the shortcuts that make a table quick.
    Compose run --rm osrm-tools osrm-extract -p /opt/car.lua /data/kyiv.osm.pbf
    Compose run --rm osrm-tools osrm-contract /data/kyiv.osrm

    $megabytes = [math]::Round((Get-ChildItem osrm | Measure-Object Length -Sum).Sum / 1MB)
    Write-Host ''
    Write-Host "The roads are in osrm/, $megabytes MB. To use them, put into .env:"
    Write-Host '  COMPOSE_PROFILES=osrm'
    Write-Host '  OSRM_URL=http://osrm:5000'
    Write-Host 'and start the stack: docker compose up --build --scale solver=4'
} finally {
    Pop-Location
}
