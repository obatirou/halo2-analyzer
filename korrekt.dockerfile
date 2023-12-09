FROM rustlang/rust:nightly-slim

# Install Python3.10
RUN apt-get update && apt-get install -y python3.10 python3-pip

# Install java, gmp, libpgm, git-all, curl and cmake
RUN apt-get install -y default-jre libgmp-dev libpgm-dev git curl cmake

# Export GMP_LIB
ENV GMP_LIB=/usr/lib/aarch64-linux-gnu/libgmp.a

# Install tomli and pyparsing
RUN python3 -m pip install tomli pyparsing

# Install cvc5
RUN git clone https://github.com/cvc5/cvc5 && cd /cvc5 && ./configure.sh --cocoa --auto-download && cd /cvc5/build && make -j4 && make check && make install

# Move cvc5 to /usr/bin
RUN mv /cvc5/build/bin/cvc5 /usr/bin/cvc5

# Clean image
# Delete cvc5 directory
RUN rm -rf /cvc5
# Uninstall java, gmp, libpgm, git-all, curl and cmake
RUN apt-get remove -y default-jre libgmp-dev libpgm-dev git curl cmake

# Copy the current directory contents into the container at /app
COPY . /app

# cd into the app directory and in korrekt directory
WORKDIR /app/korrekt