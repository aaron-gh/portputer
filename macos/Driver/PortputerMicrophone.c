// Portputer's audio driver: an Audio Server plug-in (a user-space audio
// driver, loaded by coreaudiod) with three devices.
//
// - "Portputer Microphone", an input device apps record from.
// - "Portputer Microphone Feed", a hidden output device. Portputer's server
//   plays the microphone of the connected phone or computer into it.
//   Both run on one clock, so a frame written to the feed at sample time T is
//   read from the microphone at sample time T. Frames nothing was written for
//   read as silence. 48 kHz, mono, 32-bit float.
// - "Portputer Speakers", a hidden stereo output device, 44.1 or 48 kHz. At
//   the login window, where macOS lets no one record system audio, Portputer
//   makes it the default output (beside the speakers) and this driver passes
//   what's played to it to Portputer's helper daemon in Mach messages.
//
// Structured after Apple's NullAudio sample driver.

#include <CoreAudio/AudioServerPlugIn.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <servers/bootstrap.h>
#include <stddef.h>
#include <stdatomic.h>
#include <string.h>
#include <unistd.h>

enum {
    kPlugIn = kAudioObjectPlugInObject,
    kMic = 2,
    kMicStream = 3,
    kFeed = 4,
    kFeedStream = 5,
    kSpeakers = 6,
    kSpeakersStream = 7,
};

#define kSampleRate 48000.0
#define kChannels 1
#define kBytesPerFrame (sizeof(Float32) * kChannels)
#define kRingFrames 65536
#define kZeroTimeStampPeriod 16384
#define kMicUID "io.github.aaron-gh.portputer.microphone"
#define kFeedUID "io.github.aaron-gh.portputer.microphone-feed"
#define kSpeakersUID "io.github.aaron-gh.portputer.speakers"
#define kModelUID "io.github.aaron-gh.portputer.microphone-model"
#define kSpeakersChannels 2
// The helper daemon's Mach service, listed in Info.plist so coreaudiod's
// sandbox lets this driver look it up.
#define kAudioService "io.github.aaron-gh.portputer.audio"
#define kAudioMessageID 0x50505331
#define kChunkFrames 480

static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static UInt32 gRefCount = 0;
static AudioServerPlugInHostRef gHost = NULL;
static Float64 gTicksPerSecond = 0;

// A clock: the zero time stamps of a device that's running.
typedef struct {
    Float64 ticksPerFrame;
    UInt64 anchorHostTime;
    UInt64 timeStamps;
} Clock;

// The microphone and the feed share one clock; the speakers have their own,
// at their own rate.
static UInt32 gRunning[3] = {0, 0, 0};
static Clock gClock = {0};
static Clock gSpeakersClock = {0};
static Float64 gSpeakersRate = 48000.0;
static Float64 gSpeakersPendingRate = 48000.0;

static Float32 gRing[kRingFrames * kChannels];
// One past the last sample time written to the feed.
static _Atomic SInt64 gWrittenEnd = 0;

// What's played to the speakers, for the sender thread: frames written and
// frames sent, counted from the start.
static Float32 gSpeakersRing[kRingFrames * kSpeakersChannels];
static _Atomic UInt64 gSpeakersWritten = 0;
static UInt64 gSpeakersSent = 0;
static pthread_t gSender;
static Boolean gSenderStarted = false;

static Boolean IsDevice(AudioObjectID id) { return id == kMic || id == kFeed || id == kSpeakers; }
static Boolean IsStream(AudioObjectID id) { return id == kMicStream || id == kFeedStream || id == kSpeakersStream; }
static int DeviceIndex(AudioObjectID id) { return id == kMic ? 0 : id == kFeed ? 1 : 2; }
static Boolean IsSpeakers(AudioObjectID id) { return id == kSpeakers || id == kSpeakersStream; }

static AudioStreamBasicDescription FormatWith(Float64 rate, UInt32 channels) {
    AudioStreamBasicDescription f = {0};
    f.mSampleRate = rate;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian | kAudioFormatFlagIsPacked;
    f.mBytesPerPacket = sizeof(Float32) * channels;
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = sizeof(Float32) * channels;
    f.mChannelsPerFrame = channels;
    f.mBitsPerChannel = 32;
    return f;
}

static AudioStreamBasicDescription Format(void) { return FormatWith(kSampleRate, kChannels); }

// The format of a device or its stream.
static AudioStreamBasicDescription FormatOf(AudioObjectID object) {
    return IsSpeakers(object) ? FormatWith(gSpeakersRate, kSpeakersChannels) : Format();
}

static const Float64 kSpeakersRates[] = {44100.0, 48000.0};

// ---- Sending the speakers' audio to the helper ----

typedef struct {
    mach_msg_header_t header;
    UInt32 rate;
    UInt32 channels;
    UInt32 frames;
    Float32 samples[kChunkFrames * kSpeakersChannels];
} AudioMessage;

// Sends what's been played to the speakers every 5 ms. Messages are
// dropped, never waited for, when the helper isn't there or is behind.
static void* Sender(void* unused) {
    (void)unused;
    mach_port_t port = MACH_PORT_NULL;
    UInt32 retry = 0;
    AudioMessage message;
    for (;;) {
        usleep(5000);
        UInt64 written = atomic_load(&gSpeakersWritten);
        // Too far behind: skip to the newest half second.
        if (written - gSpeakersSent > kRingFrames / 2) gSpeakersSent = written - 24000;
        if (port == MACH_PORT_NULL) {
            if (retry > 0) {
                retry--;
                gSpeakersSent = written;
                continue;
            }
            if (bootstrap_look_up(bootstrap_port, kAudioService, &port) != KERN_SUCCESS) {
                port = MACH_PORT_NULL;
                retry = 200;
                gSpeakersSent = written;
                continue;
            }
        }
        while (gSpeakersSent < written) {
            UInt32 frames = (UInt32)(written - gSpeakersSent);
            if (frames > kChunkFrames) frames = kChunkFrames;
            for (UInt32 i = 0; i < frames; i++) {
                UInt64 t = (gSpeakersSent + i) % kRingFrames;
                memcpy(&message.samples[i * kSpeakersChannels], &gSpeakersRing[t * kSpeakersChannels], sizeof(Float32) * kSpeakersChannels);
            }
            message.rate = (UInt32)gSpeakersRate;
            message.channels = kSpeakersChannels;
            message.frames = frames;
            UInt32 size = (UInt32)(offsetof(AudioMessage, samples) + frames * kSpeakersChannels * sizeof(Float32));
            message.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
            message.header.msgh_size = size;
            message.header.msgh_remote_port = port;
            message.header.msgh_local_port = MACH_PORT_NULL;
            message.header.msgh_voucher_port = MACH_PORT_NULL;
            message.header.msgh_id = kAudioMessageID;
            kern_return_t result = mach_msg(&message.header, MACH_SEND_MSG | MACH_SEND_TIMEOUT, size, 0, MACH_PORT_NULL, 0, MACH_PORT_NULL);
            gSpeakersSent += frames;
            if (result == MACH_SEND_INVALID_DEST) {
                mach_port_deallocate(mach_task_self(), port);
                port = MACH_PORT_NULL;
                break;
            }
        }
    }
    return NULL;
}

// ---- IUnknown ----

static HRESULT QueryInterface(void* driver, REFIID iid, LPVOID* out);
static ULONG AddRef(void* driver);
static ULONG Release(void* driver);
static OSStatus Initialize(AudioServerPlugInDriverRef driver, AudioServerPlugInHostRef host);
static OSStatus CreateDevice(AudioServerPlugInDriverRef driver, CFDictionaryRef description, const AudioServerPlugInClientInfo* client, AudioObjectID* outID);
static OSStatus DestroyDevice(AudioServerPlugInDriverRef driver, AudioObjectID device);
static OSStatus AddDeviceClient(AudioServerPlugInDriverRef driver, AudioObjectID device, const AudioServerPlugInClientInfo* client);
static OSStatus RemoveDeviceClient(AudioServerPlugInDriverRef driver, AudioObjectID device, const AudioServerPlugInClientInfo* client);
static OSStatus PerformConfigurationChange(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt64 action, void* info);
static OSStatus AbortConfigurationChange(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt64 action, void* info);
static Boolean HasProperty(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* address);
static OSStatus IsPropertySettable(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* address, Boolean* out);
static OSStatus GetPropertyDataSize(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* address, UInt32 qualifierSize, const void* qualifier, UInt32* outSize);
static OSStatus GetPropertyData(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* address, UInt32 qualifierSize, const void* qualifier, UInt32 inSize, UInt32* outSize, void* outData);
static OSStatus SetPropertyData(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* address, UInt32 qualifierSize, const void* qualifier, UInt32 inSize, const void* inData);
static OSStatus StartIO(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client);
static OSStatus StopIO(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client);
static OSStatus GetZeroTimeStamp(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, Float64* sampleTime, UInt64* hostTime, UInt64* seed);
static OSStatus WillDoIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, Boolean* willDo, Boolean* inPlace);
static OSStatus BeginIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo* cycle);
static OSStatus DoIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, AudioObjectID stream, UInt32 client, UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo* cycle, void* main, void* secondary);
static OSStatus EndIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo* cycle);

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    QueryInterface, AddRef, Release,
    Initialize, CreateDevice, DestroyDevice, AddDeviceClient, RemoveDeviceClient,
    PerformConfigurationChange, AbortConfigurationChange,
    HasProperty, IsPropertySettable, GetPropertyDataSize, GetPropertyData, SetPropertyData,
    StartIO, StopIO, GetZeroTimeStamp,
    WillDoIOOperation, BeginIOOperation, DoIOOperation, EndIOOperation,
};
static AudioServerPlugInDriverInterface* gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriver = &gInterfacePtr;

// The factory named in Info.plist.
void* PortputerMicrophone_Create(CFAllocatorRef allocator, CFUUIDRef type) {
    (void)allocator;
    if (CFEqual(type, kAudioServerPlugInTypeUUID)) {
        return gDriver;
    }
    return NULL;
}

static HRESULT QueryInterface(void* driver, REFIID iid, LPVOID* out) {
    if (driver != gDriver || out == NULL) return kAudioHardwareBadObjectError;
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, iid);
    HRESULT result = E_NOINTERFACE;
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        pthread_mutex_lock(&gLock);
        gRefCount++;
        pthread_mutex_unlock(&gLock);
        *out = gDriver;
        result = S_OK;
    }
    CFRelease(requested);
    return result;
}

static ULONG AddRef(void* driver) {
    if (driver != gDriver) return 0;
    pthread_mutex_lock(&gLock);
    ULONG n = ++gRefCount;
    pthread_mutex_unlock(&gLock);
    return n;
}

static ULONG Release(void* driver) {
    if (driver != gDriver) return 0;
    pthread_mutex_lock(&gLock);
    if (gRefCount > 0) gRefCount--;
    ULONG n = gRefCount;
    pthread_mutex_unlock(&gLock);
    return n;
}

// ---- Basic operations ----

static OSStatus Initialize(AudioServerPlugInDriverRef driver, AudioServerPlugInHostRef host) {
    if (driver != gDriver) return kAudioHardwareBadObjectError;
    gHost = host;
    mach_timebase_info_data_t timebase;
    mach_timebase_info(&timebase);
    gTicksPerSecond = 1e9 * (Float64)timebase.denom / (Float64)timebase.numer;
    gClock.ticksPerFrame = gTicksPerSecond / kSampleRate;
    gSpeakersClock.ticksPerFrame = gTicksPerSecond / gSpeakersRate;
    return 0;
}

static OSStatus CreateDevice(AudioServerPlugInDriverRef d, CFDictionaryRef desc, const AudioServerPlugInClientInfo* c, AudioObjectID* out) {
    (void)d; (void)desc; (void)c; (void)out;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus DestroyDevice(AudioServerPlugInDriverRef d, AudioObjectID dev) {
    (void)d; (void)dev;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus AddDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID dev, const AudioServerPlugInClientInfo* c) {
    (void)c;
    return (d == gDriver && IsDevice(dev)) ? 0 : kAudioHardwareBadObjectError;
}

static OSStatus RemoveDeviceClient(AudioServerPlugInDriverRef d, AudioObjectID dev, const AudioServerPlugInClientInfo* c) {
    (void)c;
    return (d == gDriver && IsDevice(dev)) ? 0 : kAudioHardwareBadObjectError;
}

static OSStatus PerformConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt64 a, void* i) {
    (void)a; (void)i;
    if (d != gDriver || !IsDevice(dev)) return kAudioHardwareBadObjectError;
    if (dev == kSpeakers) {
        pthread_mutex_lock(&gLock);
        gSpeakersRate = gSpeakersPendingRate;
        gSpeakersClock.ticksPerFrame = gTicksPerSecond / gSpeakersRate;
        pthread_mutex_unlock(&gLock);
    }
    return 0;
}

static OSStatus AbortConfigurationChange(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt64 a, void* i) {
    (void)a; (void)i;
    return (d == gDriver && IsDevice(dev)) ? 0 : kAudioHardwareBadObjectError;
}

// ---- Properties ----

static Boolean HasProperty(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* a) {
    (void)client;
    if (driver != gDriver || a == NULL) return false;
    switch (object) {
    case kPlugIn:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioObjectPropertyManufacturer:
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList:
        case kAudioPlugInPropertyTranslateUIDToDevice:
        case kAudioPlugInPropertyResourceBundle:
            return true;
        }
        return false;
    case kMic:
    case kFeed:
    case kSpeakers:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioObjectPropertyName:
        case kAudioObjectPropertyManufacturer:
        case kAudioObjectPropertyOwnedObjects:
        case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID:
        case kAudioDevicePropertyTransportType:
        case kAudioDevicePropertyRelatedDevices:
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceIsRunning:
        case kAudioObjectPropertyControlList:
        case kAudioDevicePropertyNominalSampleRate:
        case kAudioDevicePropertyAvailableNominalSampleRates:
        case kAudioDevicePropertyIsHidden:
        case kAudioDevicePropertyZeroTimeStampPeriod:
        case kAudioDevicePropertyStreams:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyPreferredChannelsForStereo:
            return true;
        }
        return false;
    case kMicStream:
    case kFeedStream:
    case kSpeakersStream:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioObjectPropertyOwnedObjects:
        case kAudioStreamPropertyIsActive:
        case kAudioStreamPropertyDirection:
        case kAudioStreamPropertyTerminalType:
        case kAudioStreamPropertyStartingChannel:
        case kAudioStreamPropertyLatency:
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats:
            return true;
        }
        return false;
    }
    return false;
}

static OSStatus IsPropertySettable(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* a, Boolean* out) {
    if (driver != gDriver || a == NULL || out == NULL) return kAudioHardwareIllegalOperationError;
    if (!HasProperty(driver, object, client, a)) return kAudioHardwareUnknownPropertyError;
    // Formats and the sample rate are "settable" to their one value, as
    // some apps set them before recording.
    *out = IsStream(object) && (a->mSelector == kAudioStreamPropertyVirtualFormat || a->mSelector == kAudioStreamPropertyPhysicalFormat || a->mSelector == kAudioStreamPropertyIsActive);
    if (IsDevice(object) && a->mSelector == kAudioDevicePropertyNominalSampleRate) *out = true;
    return 0;
}

// The streams a device has in a scope.
static UInt32 DeviceStreams(AudioObjectID device, AudioObjectPropertyScope scope, AudioObjectID* out) {
    AudioObjectID stream = device == kMic ? kMicStream : device == kFeed ? kFeedStream : kSpeakersStream;
    AudioObjectPropertyScope own = device == kMic ? kAudioObjectPropertyScopeInput : kAudioObjectPropertyScopeOutput;
    if (scope == kAudioObjectPropertyScopeGlobal || scope == own) {
        if (out) out[0] = stream;
        return 1;
    }
    return 0;
}

static OSStatus GetPropertyDataSize(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* a, UInt32 qs, const void* q, UInt32* outSize) {
    (void)qs; (void)q;
    if (driver != gDriver || a == NULL || outSize == NULL) return kAudioHardwareIllegalOperationError;
    if (!HasProperty(driver, object, client, a)) return kAudioHardwareUnknownPropertyError;
    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass:
    case kAudioObjectPropertyClass:
        *outSize = sizeof(AudioClassID);
        return 0;
    case kAudioObjectPropertyOwner:
    case kAudioPlugInPropertyTranslateUIDToDevice:
        *outSize = sizeof(AudioObjectID);
        return 0;
    case kAudioObjectPropertyName:
    case kAudioObjectPropertyManufacturer:
    case kAudioDevicePropertyDeviceUID:
    case kAudioDevicePropertyModelUID:
    case kAudioPlugInPropertyResourceBundle:
        *outSize = sizeof(CFStringRef);
        return 0;
    case kAudioObjectPropertyOwnedObjects:
        if (object == kPlugIn) *outSize = 3 * sizeof(AudioObjectID);
        else if (IsDevice(object)) *outSize = DeviceStreams(object, a->mScope, NULL) * sizeof(AudioObjectID);
        else *outSize = 0;
        return 0;
    case kAudioPlugInPropertyDeviceList:
        *outSize = 3 * sizeof(AudioObjectID);
        return 0;
    case kAudioDevicePropertyStreams:
        *outSize = DeviceStreams(object, a->mScope, NULL) * sizeof(AudioObjectID);
        return 0;
    case kAudioDevicePropertyRelatedDevices:
        *outSize = sizeof(AudioObjectID);
        return 0;
    case kAudioObjectPropertyControlList:
        *outSize = 0;
        return 0;
    case kAudioDevicePropertyNominalSampleRate:
        *outSize = sizeof(Float64);
        return 0;
    case kAudioDevicePropertyAvailableNominalSampleRates:
        *outSize = (IsSpeakers(object) ? 2 : 1) * sizeof(AudioValueRange);
        return 0;
    case kAudioDevicePropertyPreferredChannelsForStereo:
        *outSize = 2 * sizeof(UInt32);
        return 0;
    case kAudioStreamPropertyVirtualFormat:
    case kAudioStreamPropertyPhysicalFormat:
        *outSize = sizeof(AudioStreamBasicDescription);
        return 0;
    case kAudioStreamPropertyAvailableVirtualFormats:
    case kAudioStreamPropertyAvailablePhysicalFormats:
        *outSize = (IsSpeakers(object) ? 2 : 1) * sizeof(AudioStreamRangedDescription);
        return 0;
    default:
        *outSize = sizeof(UInt32);
        return 0;
    }
}

#define PUT(type, value)                                          \
    do {                                                          \
        if (inSize < sizeof(type)) return kAudioHardwareBadPropertySizeError; \
        *(type*)outData = (value);                                \
        *outSize = sizeof(type);                                  \
        return 0;                                                 \
    } while (0)

static OSStatus GetPropertyData(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* a, UInt32 qs, const void* q, UInt32 inSize, UInt32* outSize, void* outData) {
    if (driver != gDriver || a == NULL || outSize == NULL || outData == NULL) return kAudioHardwareIllegalOperationError;
    if (!HasProperty(driver, object, client, a)) return kAudioHardwareUnknownPropertyError;
    Boolean mic = object == kMic || object == kMicStream;

    if (object == kPlugIn) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID);
        case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioPlugInClassID);
        case kAudioObjectPropertyOwner: PUT(AudioObjectID, kAudioObjectUnknown);
        case kAudioObjectPropertyManufacturer: PUT(CFStringRef, CFSTR("Portputer"));
        case kAudioPlugInPropertyResourceBundle: PUT(CFStringRef, CFSTR(""));
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList: {
            UInt32 n = inSize / sizeof(AudioObjectID);
            AudioObjectID* ids = outData;
            UInt32 count = 0;
            if (n > 0) ids[count++] = kMic;
            if (n > 1) ids[count++] = kFeed;
            if (n > 2) ids[count++] = kSpeakers;
            *outSize = count * sizeof(AudioObjectID);
            return 0;
        }
        case kAudioPlugInPropertyTranslateUIDToDevice: {
            if (qs != sizeof(CFStringRef) || q == NULL) return kAudioHardwareBadPropertySizeError;
            CFStringRef uid = *(const CFStringRef*)q;
            AudioObjectID found = kAudioObjectUnknown;
            if (CFStringCompare(uid, CFSTR(kMicUID), 0) == kCFCompareEqualTo) found = kMic;
            if (CFStringCompare(uid, CFSTR(kFeedUID), 0) == kCFCompareEqualTo) found = kFeed;
            if (CFStringCompare(uid, CFSTR(kSpeakersUID), 0) == kCFCompareEqualTo) found = kSpeakers;
            PUT(AudioObjectID, found);
        }
        }
        return kAudioHardwareUnknownPropertyError;
    }

    if (IsDevice(object)) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID);
        case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioDeviceClassID);
        case kAudioObjectPropertyOwner: PUT(AudioObjectID, kPlugIn);
        case kAudioObjectPropertyName: PUT(CFStringRef, mic ? CFSTR("Portputer Microphone") : object == kFeed ? CFSTR("Portputer Microphone Feed") : CFSTR("Portputer Speakers"));
        case kAudioObjectPropertyManufacturer: PUT(CFStringRef, CFSTR("Portputer"));
        case kAudioDevicePropertyDeviceUID: PUT(CFStringRef, mic ? CFSTR(kMicUID) : object == kFeed ? CFSTR(kFeedUID) : CFSTR(kSpeakersUID));
        case kAudioDevicePropertyModelUID: PUT(CFStringRef, CFSTR(kModelUID));
        case kAudioDevicePropertyTransportType: PUT(UInt32, kAudioDeviceTransportTypeVirtual);
        case kAudioDevicePropertyRelatedDevices: PUT(AudioObjectID, object);
        case kAudioDevicePropertyClockDomain: PUT(UInt32, 0);
        case kAudioDevicePropertyDeviceIsAlive: PUT(UInt32, 1);
        case kAudioDevicePropertyDeviceIsRunning: {
            pthread_mutex_lock(&gLock);
            UInt32 running = gRunning[DeviceIndex(object)] > 0;
            pthread_mutex_unlock(&gLock);
            PUT(UInt32, running);
        }
        case kAudioDevicePropertyDeviceCanBeDefaultDevice: PUT(UInt32, (mic || object == kSpeakers) ? 1 : 0);
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: PUT(UInt32, object == kSpeakers ? 1 : 0);
        case kAudioDevicePropertyLatency: PUT(UInt32, 0);
        case kAudioDevicePropertySafetyOffset: PUT(UInt32, 0);
        case kAudioDevicePropertyIsHidden: PUT(UInt32, mic ? 0 : 1);
        case kAudioDevicePropertyZeroTimeStampPeriod: PUT(UInt32, kZeroTimeStampPeriod);
        case kAudioDevicePropertyNominalSampleRate: PUT(Float64, object == kSpeakers ? gSpeakersRate : kSampleRate);
        case kAudioDevicePropertyAvailableNominalSampleRates: {
            if (object != kSpeakers) {
                AudioValueRange range = {kSampleRate, kSampleRate};
                PUT(AudioValueRange, range);
            }
            UInt32 n = inSize / sizeof(AudioValueRange);
            if (n > 2) n = 2;
            for (UInt32 i = 0; i < n; i++) {
                ((AudioValueRange*)outData)[i].mMinimum = kSpeakersRates[i];
                ((AudioValueRange*)outData)[i].mMaximum = kSpeakersRates[i];
            }
            *outSize = n * sizeof(AudioValueRange);
            return 0;
        }
        case kAudioObjectPropertyControlList:
            *outSize = 0;
            return 0;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioDevicePropertyStreams: {
            AudioObjectID ids[1];
            UInt32 n = DeviceStreams(object, a->mScope, ids);
            if (inSize < n * sizeof(AudioObjectID)) n = inSize / sizeof(AudioObjectID);
            memcpy(outData, ids, n * sizeof(AudioObjectID));
            *outSize = n * sizeof(AudioObjectID);
            return 0;
        }
        case kAudioDevicePropertyPreferredChannelsForStereo: {
            if (inSize < 2 * sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            ((UInt32*)outData)[0] = 1;
            ((UInt32*)outData)[1] = object == kSpeakers ? 2 : 1;
            *outSize = 2 * sizeof(UInt32);
            return 0;
        }
        }
        return kAudioHardwareUnknownPropertyError;
    }

    if (IsStream(object)) {
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass: PUT(AudioClassID, kAudioObjectClassID);
        case kAudioObjectPropertyClass: PUT(AudioClassID, kAudioStreamClassID);
        case kAudioObjectPropertyOwner: PUT(AudioObjectID, mic ? kMic : object == kFeedStream ? kFeed : kSpeakers);
        case kAudioObjectPropertyOwnedObjects:
            *outSize = 0;
            return 0;
        case kAudioStreamPropertyIsActive: PUT(UInt32, 1);
        case kAudioStreamPropertyDirection: PUT(UInt32, mic ? 1 : 0);
        case kAudioStreamPropertyTerminalType: PUT(UInt32, mic ? kAudioStreamTerminalTypeMicrophone : object == kSpeakersStream ? kAudioStreamTerminalTypeSpeaker : kAudioStreamTerminalTypeLine);
        case kAudioStreamPropertyStartingChannel: PUT(UInt32, 1);
        case kAudioStreamPropertyLatency: PUT(UInt32, 0);
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: PUT(AudioStreamBasicDescription, FormatOf(object));
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: {
            if (object != kSpeakersStream) {
                AudioStreamRangedDescription ranged;
                ranged.mFormat = Format();
                ranged.mSampleRateRange.mMinimum = kSampleRate;
                ranged.mSampleRateRange.mMaximum = kSampleRate;
                PUT(AudioStreamRangedDescription, ranged);
            }
            UInt32 n = inSize / sizeof(AudioStreamRangedDescription);
            if (n > 2) n = 2;
            for (UInt32 i = 0; i < n; i++) {
                AudioStreamRangedDescription* ranged = &((AudioStreamRangedDescription*)outData)[i];
                ranged->mFormat = FormatWith(kSpeakersRates[i], kSpeakersChannels);
                ranged->mSampleRateRange.mMinimum = kSpeakersRates[i];
                ranged->mSampleRateRange.mMaximum = kSpeakersRates[i];
            }
            *outSize = n * sizeof(AudioStreamRangedDescription);
            return 0;
        }
        }
        return kAudioHardwareUnknownPropertyError;
    }
    return kAudioHardwareBadObjectError;
}

// A change of the speakers' rate goes through the host, which stops IO and
// calls PerformConfigurationChange.
static OSStatus SetSpeakersRate(Float64 rate) {
    if (rate != kSpeakersRates[0] && rate != kSpeakersRates[1]) return kAudioDeviceUnsupportedFormatError;
    gSpeakersPendingRate = rate;
    if (gHost == NULL) return 0;
    return gHost->RequestDeviceConfigurationChange(gHost, kSpeakers, 0, NULL);
}

static OSStatus SetPropertyData(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t client, const AudioObjectPropertyAddress* a, UInt32 qs, const void* q, UInt32 inSize, const void* inData) {
    (void)qs; (void)q;
    if (driver != gDriver || a == NULL || inData == NULL) return kAudioHardwareIllegalOperationError;
    if (!HasProperty(driver, object, client, a)) return kAudioHardwareUnknownPropertyError;
    switch (a->mSelector) {
    case kAudioStreamPropertyIsActive:
        return 0;
    case kAudioStreamPropertyVirtualFormat:
    case kAudioStreamPropertyPhysicalFormat: {
        if (inSize != sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
        const AudioStreamBasicDescription* f = inData;
        AudioStreamBasicDescription ours = FormatOf(object);
        Boolean same = f->mFormatID == ours.mFormatID && f->mChannelsPerFrame == ours.mChannelsPerFrame && f->mBitsPerChannel == ours.mBitsPerChannel;
        if (!same) return kAudioDeviceUnsupportedFormatError;
        if (f->mSampleRate == ours.mSampleRate) return 0;
        return IsSpeakers(object) ? SetSpeakersRate(f->mSampleRate) : kAudioDeviceUnsupportedFormatError;
    }
    case kAudioDevicePropertyNominalSampleRate: {
        if (inSize != sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
        Float64 rate = *(const Float64*)inData;
        if (object == kSpeakers) return rate == gSpeakersRate ? 0 : SetSpeakersRate(rate);
        return rate == kSampleRate ? 0 : kAudioDeviceUnsupportedFormatError;
    }
    }
    return kAudioHardwareUnsupportedOperationError;
}

// ---- IO ----

static OSStatus StartIO(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client) {
    (void)client;
    if (driver != gDriver || !IsDevice(device)) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    if (device == kSpeakers) {
        if (gRunning[2] == 0) {
            gSpeakersClock.anchorHostTime = mach_absolute_time();
            gSpeakersClock.timeStamps = 0;
        }
        if (!gSenderStarted && pthread_create(&gSender, NULL, Sender, NULL) == 0) {
            pthread_detach(gSender);
            gSenderStarted = true;
        }
    } else if (gRunning[0] == 0 && gRunning[1] == 0) {
        gClock.anchorHostTime = mach_absolute_time();
        gClock.timeStamps = 0;
        // Sample times start again from zero.
        atomic_store(&gWrittenEnd, 0);
    }
    gRunning[DeviceIndex(device)]++;
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus StopIO(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client) {
    (void)client;
    if (driver != gDriver || !IsDevice(device)) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    int i = DeviceIndex(device);
    if (gRunning[i] > 0) gRunning[i]--;
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus GetZeroTimeStamp(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, Float64* sampleTime, UInt64* hostTime, UInt64* seed) {
    (void)client;
    if (driver != gDriver || !IsDevice(device)) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    Clock* clock = device == kSpeakers ? &gSpeakersClock : &gClock;
    UInt64 now = mach_absolute_time();
    Float64 ticksPerPeriod = clock->ticksPerFrame * kZeroTimeStampPeriod;
    UInt64 next = clock->anchorHostTime + (UInt64)((clock->timeStamps + 1) * ticksPerPeriod);
    if (next <= now) clock->timeStamps++;
    *sampleTime = clock->timeStamps * kZeroTimeStampPeriod;
    *hostTime = clock->anchorHostTime + (UInt64)(clock->timeStamps * ticksPerPeriod);
    *seed = 1;
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus WillDoIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, Boolean* willDo, Boolean* inPlace) {
    (void)client;
    if (driver != gDriver || !IsDevice(device)) return kAudioHardwareBadObjectError;
    Boolean yes = (device == kMic && operation == kAudioServerPlugInIOOperationReadInput) || (device != kMic && operation == kAudioServerPlugInIOOperationWriteMix);
    if (willDo) *willDo = yes;
    if (inPlace) *inPlace = true;
    return 0;
}

static OSStatus BeginIOOperation(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 c, UInt32 o, UInt32 f, const AudioServerPlugInIOCycleInfo* i) {
    (void)c; (void)o; (void)f; (void)i;
    return (d == gDriver && IsDevice(dev)) ? 0 : kAudioHardwareBadObjectError;
}

static OSStatus EndIOOperation(AudioServerPlugInDriverRef d, AudioObjectID dev, UInt32 c, UInt32 o, UInt32 f, const AudioServerPlugInIOCycleInfo* i) {
    (void)c; (void)o; (void)f; (void)i;
    return (d == gDriver && IsDevice(dev)) ? 0 : kAudioHardwareBadObjectError;
}

static OSStatus DoIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, AudioObjectID stream, UInt32 client, UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo* cycle, void* mainBuffer, void* secondary) {
    (void)client; (void)stream; (void)secondary;
    if (driver != gDriver || !IsDevice(device) || cycle == NULL || mainBuffer == NULL) return kAudioHardwareBadObjectError;
    Float32* buffer = mainBuffer;
    if (device == kFeed && operation == kAudioServerPlugInIOOperationWriteMix) {
        SInt64 start = (SInt64)cycle->mOutputTime.mSampleTime;
        for (UInt32 i = 0; i < frames; i++) {
            SInt64 t = start + i;
            memcpy(&gRing[(t % kRingFrames) * kChannels], &buffer[i * kChannels], kBytesPerFrame);
        }
        SInt64 end = start + frames;
        SInt64 previous = atomic_load(&gWrittenEnd);
        while (end > previous && !atomic_compare_exchange_weak(&gWrittenEnd, &previous, end)) {}
    } else if (device == kSpeakers && operation == kAudioServerPlugInIOOperationWriteMix) {
        UInt64 at = atomic_load(&gSpeakersWritten);
        for (UInt32 i = 0; i < frames; i++) {
            memcpy(&gSpeakersRing[((at + i) % kRingFrames) * kSpeakersChannels], &buffer[i * kSpeakersChannels], sizeof(Float32) * kSpeakersChannels);
        }
        atomic_store(&gSpeakersWritten, at + frames);
    } else if (device == kMic && operation == kAudioServerPlugInIOOperationReadInput) {
        SInt64 start = (SInt64)cycle->mInputTime.mSampleTime;
        SInt64 written = atomic_load(&gWrittenEnd);
        for (UInt32 i = 0; i < frames; i++) {
            SInt64 t = start + i;
            if (t >= 0 && t < written && t >= written - kRingFrames) {
                memcpy(&buffer[i * kChannels], &gRing[(t % kRingFrames) * kChannels], kBytesPerFrame);
            } else {
                memset(&buffer[i * kChannels], 0, kBytesPerFrame);
            }
        }
    }
    return 0;
}
