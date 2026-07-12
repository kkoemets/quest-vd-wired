LOCAL_PATH := $(call my-dir)
HEV_PATH := $(LOCAL_PATH)/../../../../.deps/hev-socks5-tunnel

ifeq (,$(wildcard $(HEV_PATH)/Android.mk))
$(error HEV dependency missing; run android-v4/scripts/fetch-hev.sh)
endif

include $(HEV_PATH)/Android.mk
