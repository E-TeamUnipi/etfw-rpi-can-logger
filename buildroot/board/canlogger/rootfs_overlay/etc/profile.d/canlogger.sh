# shell conveniences for debugging on the logger
alias log='logread -f'
alias rw='mount -o remount,rw / && echo "rootfs is WRITABLE, run ro when done"'
alias ro='sync && mount -o remount,ro / && echo "rootfs read-only"'
alias bootrw='mount -o remount,rw /boot'
alias bootro='sync && mount -o remount,ro /boot'
echo "CAN logger - status: canring /dev/mmcblk0p3 list | logs: logread -f | edit rootfs: rw ... ro"
