// Portputer Microphone: an Audio Server plug-in (a user-space audio driver,
// loaded by coreaudiod) with two devices sharing one buffer.
//
// - "Portputer Microphone", an input device apps record from.
// - "Portputer Microphone Feed", a hidden output device. Portputer's server
//   plays the microphone of the connected phone or computer into it.
//
// Both run on one clock, so a frame written to the feed at sample time T is
// read from the microphone at sample time T. Frames nothing was written for
// read as silence. 48 kHz, mono, 32-bit float. Structured after Apple's
// NullAudio sample driver.

#include <CoreAudio/AudioServerPlugIn.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <string.h>

enum {
    kPlugIn = kAudioObjectPlugInObject,
    kMic = 2,
    kMicStream = 3,
    kFeed = 4,
    kFeedStream = 5,
};

#define kSampleRate 48000.0
#define kChannels 1
#define kBytesPerFrame (sizeof(Float32) * kChannels)
#define kRingFrames 65536
#define kZeroTimeStampPeriod 16384
#define kMicUID "io.github.aaron-gh.portputer.microphone"
#define kFeedUID "io.github.aaron-gh.portputer.microphone-feed"
#define kModelUID "io.github.aaron-gh.portputer.microphone-model"

static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static UInt32 gRefCount = 0;
static AudioServerPlugInHostRef gHost = NULL;

// The shared clock: running counts per device, and the zero time stamps.
static UInt32 gRunning[2] = {0, 0};
static Float64 gTicksPerFrame = 0;
static UInt64 gAnchorHostTime = 0;
static UInt64 gTimeStamps = 0;

static Float32 gRing[kRingFrames * kChannels];
// One past the last sample time written to the feed.
static _Atomic SInt64 gWrittenEnd = 0;

static Boolean IsDevice(AudioObjectID id) { return id == kMic || id == kFeed; }
static Boolean IsStream(AudioObjectID id) { return id == kMicStream || id == kFeedStream; }
static int DeviceIndex(AudioObjectID id) { return id == kMic ? 0 : 1; }

static AudioStreamBasicDescription Format(void) {
    AudioStreamBasicDescription f = {0};
    f.mSampleRate = kSampleRate;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian | kAudioFormatFlagIsPacked;
    f.mBytesPerPacket = kBytesPerFrame;
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = kBytesPerFrame;
    f.mChannelsPerFrame = kChannels;
    f.mBitsPerChannel = 32;
    return f;
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
    Float64 ticksPerSecond = 1e9 * (Float64)timebase.denom / (Float64)timebase.numer;
    gTicksPerFrame = ticksPerSecond / kSampleRate;
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
    return (d == gDriver && IsDevice(dev)) ? 0 : kAudioHardwareBadObjectError;
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
    AudioObjectID stream = device == kMic ? kMicStream : kFeedStream;
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
        if (object == kPlugIn) *outSize = 2 * sizeof(AudioObjectID);
        else if (IsDevice(object)) *outSize = DeviceStreams(object, a->mScope, NULL) * sizeof(AudioObjectID);
        else *outSize = 0;
        return 0;
    case kAudioPlugInPropertyDeviceList:
        *outSize = 2 * sizeof(AudioObjectID);
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
        *outSize = sizeof(AudioValueRange);
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
        *outSize = sizeof(AudioStreamRangedDescription);
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
            *outSize = count * sizeof(AudioObjectID);
            return 0;
        }
        case kAudioPlugInPropertyTranslateUIDToDevice: {
            if (qs != sizeof(CFStringRef) || q == NULL) return kAudioHardwareBadPropertySizeError;
            CFStringRef uid = *(const CFStringRef*)q;
            AudioObjectID found = kAudioObjectUnknown;
            if (CFStringCompare(uid, CFSTR(kMicUID), 0) == kCFCompareEqualTo) found = kMic;
            if (CFStringCompare(uid, CFSTR(kFeedUID), 0) == kCFCompareEqualTo) found = kFeed;
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
        case kAudioObjectPropertyName: PUT(CFStringRef, mic ? CFSTR("Portputer Microphone") : CFSTR("Portputer Microphone Feed"));
        case kAudioObjectPropertyManufacturer: PUT(CFStringRef, CFSTR("Portputer"));
        case kAudioDevicePropertyDeviceUID: PUT(CFStringRef, mic ? CFSTR(kMicUID) : CFSTR(kFeedUID));
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
        case kAudioDevicePropertyDeviceCanBeDefaultDevice: PUT(UInt32, mic ? 1 : 0);
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: PUT(UInt32, 0);
        case kAudioDevicePropertyLatency: PUT(UInt32, 0);
        case kAudioDevicePropertySafetyOffset: PUT(UInt32, 0);
        case kAudioDevicePropertyIsHidden: PUT(UInt32, mic ? 0 : 1);
        case kAudioDevicePropertyZeroTimeStampPeriod: PUT(UInt32, kZeroTimeStampPeriod);
        case kAudioDevicePropertyNominalSampleRate: PUT(Float64, kSampleRate);
        case kAudioDevicePropertyAvailableNominalSampleRates: {
            AudioValueRange range = {kSampleRate, kSampleRate};
            PUT(AudioValueRange, range);
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
            ((UInt32*)outData)[1] = 1;
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
        case kAudioObjectPropertyOwner: PUT(AudioObjectID, mic ? kMic : kFeed);
        case kAudioObjectPropertyOwnedObjects:
            *outSize = 0;
            return 0;
        case kAudioStreamPropertyIsActive: PUT(UInt32, 1);
        case kAudioStreamPropertyDirection: PUT(UInt32, mic ? 1 : 0);
        case kAudioStreamPropertyTerminalType: PUT(UInt32, mic ? kAudioStreamTerminalTypeMicrophone : kAudioStreamTerminalTypeLine);
        case kAudioStreamPropertyStartingChannel: PUT(UInt32, 1);
        case kAudioStreamPropertyLatency: PUT(UInt32, 0);
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: PUT(AudioStreamBasicDescription, Format());
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: {
            AudioStreamRangedDescription ranged;
            ranged.mFormat = Format();
            ranged.mSampleRateRange.mMinimum = kSampleRate;
            ranged.mSampleRateRange.mMaximum = kSampleRate;
            PUT(AudioStreamRangedDescription, ranged);
        }
        }
        return kAudioHardwareUnknownPropertyError;
    }
    return kAudioHardwareBadObjectError;
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
        AudioStreamBasicDescription ours = Format();
        Boolean same = f->mSampleRate == ours.mSampleRate && f->mFormatID == ours.mFormatID && f->mChannelsPerFrame == ours.mChannelsPerFrame && f->mBitsPerChannel == ours.mBitsPerChannel;
        return same ? 0 : kAudioDeviceUnsupportedFormatError;
    }
    case kAudioDevicePropertyNominalSampleRate:
        if (inSize != sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
        return *(const Float64*)inData == kSampleRate ? 0 : kAudioDeviceUnsupportedFormatError;
    }
    return kAudioHardwareUnsupportedOperationError;
}

// ---- IO ----

static OSStatus StartIO(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client) {
    (void)client;
    if (driver != gDriver || !IsDevice(device)) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gLock);
    if (gRunning[0] == 0 && gRunning[1] == 0) {
        gAnchorHostTime = mach_absolute_time();
        gTimeStamps = 0;
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
    UInt64 now = mach_absolute_time();
    Float64 ticksPerPeriod = gTicksPerFrame * kZeroTimeStampPeriod;
    UInt64 next = gAnchorHostTime + (UInt64)((gTimeStamps + 1) * ticksPerPeriod);
    if (next <= now) gTimeStamps++;
    *sampleTime = gTimeStamps * kZeroTimeStampPeriod;
    *hostTime = gAnchorHostTime + (UInt64)(gTimeStamps * ticksPerPeriod);
    *seed = 1;
    pthread_mutex_unlock(&gLock);
    return 0;
}

static OSStatus WillDoIOOperation(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, Boolean* willDo, Boolean* inPlace) {
    (void)client;
    if (driver != gDriver || !IsDevice(device)) return kAudioHardwareBadObjectError;
    Boolean yes = (device == kMic && operation == kAudioServerPlugInIOOperationReadInput) || (device == kFeed && operation == kAudioServerPlugInIOOperationWriteMix);
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
