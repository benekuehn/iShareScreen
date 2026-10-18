/* Synthetic ABI lifetime checks for Memcheck; no credentials or GPU needed. */
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <unistd.h>

extern void *iss_udp_new(int fd, size_t capacity);
extern size_t iss_udp_read(void *receiver, void *data, size_t capacity);
extern uint64_t iss_udp_dropped(void *receiver);
extern void iss_udp_free(void *receiver);

int main(void) {
    assert(iss_udp_new(-1, 1) == NULL);
    for (int cycle = 0; cycle < 10; ++cycle) {
        int input = socket(AF_INET, SOCK_DGRAM, 0);
        int output = socket(AF_INET, SOCK_DGRAM, 0);
        assert(input >= 0 && output >= 0);
        struct sockaddr_in address = {.sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
        assert(bind(input, (struct sockaddr *)&address, sizeof(address)) == 0);
        socklen_t length = sizeof(address);
        assert(getsockname(input, (struct sockaddr *)&address, &length) == 0);
        void *receiver = iss_udp_new(input, 128);
        assert(receiver);
        uint8_t packet[1200], batch[262144];
        for (unsigned i = 0; i < 50; ++i) {
            memset(packet, i, sizeof(packet));
            assert(sendto(output, packet, sizeof(packet), 0, (struct sockaddr *)&address, length) == sizeof(packet));
        }
        unsigned count = 0;
        for (unsigned attempt = 0; attempt < 200 && count < 50; ++attempt) {
            size_t size = iss_udp_read(receiver, batch, sizeof(batch));
            for (size_t pos = 0; pos < size;) {
                assert(pos + 2 <= size);
                unsigned bytes = ((unsigned)batch[pos] << 8) | batch[pos + 1];
                pos += 2;
                assert(bytes == sizeof(packet) && pos + bytes <= size);
                for (unsigned j = 0; j < bytes; ++j) assert(batch[pos+j] == count);
                pos += bytes;
                ++count;
            }
        }
        assert(count == 50 && iss_udp_dropped(receiver) == 0);
        iss_udp_free(receiver);
        /* Caller retains ownership of the original descriptor. */
        assert(getsockname(input, (struct sockaddr *)&address, &length) == 0);
        close(input);
        close(output);
    }
    puts("UDP lifecycle: 500 datagrams, 10 create/free cycles passed");
    return 0;
}
