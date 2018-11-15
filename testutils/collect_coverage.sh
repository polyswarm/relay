#!/usr/bin/env zsh

setopt BAD_PATTERN          # Print error if filename glob pat. is badly formed
setopt XTRACE               # Print commands as they are executed
unsetopt ALL_EXPORT         # Absolutely do not automatically export defined params

SCRIPT_PATH="$0"
BASEDIR=$(dirname "$0")
RELAY_DIR=$BASEDIR:h

instrument() {
    cargo cov build && cargo cov test && return 0;
    echo "Something has gone wrong compiling the crate and producing coverage files"
    exit 1
}


# Build and install kcov
build_kcov() {
    (
        cd /tmp
        # different versions of kcov contain a variety of incompatibilities, incl.
        # both instrumentation as well as a different file naming scheme.

        # rust:latest (the image we use to test this repo inside CI) pulls in an
        # older version of kcov which generates coverage reports with
        # difficult-to-extract information buryed in HTML.

        # We build from a known version here to get around this.
        wget https://github.com/polyswarm/kcov/archive/master.zip
        echo "Successfully downloaded polyswarm/kcov"
        unzip master.zip
        cd kcov-master
        mkdir build
        cd build
        cmake ..
        make
        make install
    )
}

# Build instrumented binaries and generate a coverage report
mk_coverage() {
    if [[ ("$#" -lt 1) ]]; then
        echo "mk_coverage requires a coverage report directory (COVERAGE_DESTDIR)"
        exit 1
    fi
    local -a instrumented_binary latest_report
    # glob in the kcov instrumented binary we will use to generate coverage information.
    instrumented_binary=("$RELAY_DIR"/target/cov/build/debug/*(*oc[1]))
    if [[ ${#instrumented_binary} -ne 1 ]]; then
        echo "could not find executable instrumentation file in RELAY_DIR: $RELAY_DIR. Check cargo cov output."
        exit 1
    fi
    local COVERAGE_DESTDIR="$1"
    [[ ! -d "$COVERAGE_DESTDIR" ]] && mkdir -p "$COVERAGE_DESTDIR"
    # Ubuntu includes a version of kcov too old to use
    build_kcov
    # Generate a coverage report, placing it in COVERAGE_DESTDIR
    kcov --exclude-pattern=".cargo" "$COVERAGE_DESTDIR" "$instrumented_binary"
}

# Extract & transform the coverage information generated from a kcov JSON report
extract_coverage() {
    if [[ ("$#" -lt 1) ]]; then
        echo "extract_coverage requires a coverage report directory (COVERAGE_DESTDIR)"
        exit 1
    fi

    local COVERAGE_DESTDIR="$1"

    # now glob out the directory our latest report lives in...
    latest_report=("$COVERAGE_DESTDIR"/*(oc[1]))
    if [[ ${#latest_report} -ne 1 ]]; then
        echo "Could not find coverage data in COVERAGE_DESTDIR: $COVERAGE_DESTDIR."
        exit 1
    fi

    # extract the two fields "covered_lines" and "total_lines", summing
    # them and finally dividing to get coverage pct.
    awk '/"covered_lines"/ && /"total_lines"/ { gsub(/,$/, ""); print }' "$latest_report/index.js"  |\
        jq '.covered_lines + "," + .total_lines'                                                    |\
        awk -F',' -f <(cat - <<- EOF
                    {
                        gsub(/"/, "");
                        covered+=\$1;
                        total+=\$2;
                    }

                    END {
                        if (!(covered == "" || total == "")) {
                            print "COVERAGE:" 100*(covered/total)
                        } else {
                            print "Could not build coverage data"
                        }
                    }
EOF
                      )
}

if [ "$#" -gt 0 ]; then
    local report_dir="$1"

    if [[ "$1" == "--auto" ]]; then
        report_dir="$(mktemp -d)"
    fi

    if [[ ! $(readlink -f "$report_dir") ]]; then
        echo "Invalid path supplied to $SCRIPT_PATH."
        exit 1
    fi

    echo "Using $report_dir for output report directory"
    instrument && mk_coverage "$report_dir" && extract_coverage "$report_dir"
else
    cat << EOF
${SCRIPT_PATH:t}: [OPTIONS] / outdir

    --auto Use a temporary outdir
EOF
fi
