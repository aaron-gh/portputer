// Loads PortputerMicrophone.driver as coreaudiod would, checks its
// properties, then writes a ramp to the feed and reads it from the microphone.
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>
#include <unistd.h>

#define CHECK(c, m) do { if (!(c)) { printf("FAIL: %s\n", m); return 1; } } while (0)

int main(int argc, char** argv) {
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8*)argv[1], strlen(argv[1]), true);
    CFPlugInRef plugin = CFPlugInCreate(NULL, url);
    CHECK(plugin, "load bundle");
    CFArrayRef factories = CFPlugInFindFactoriesForPlugInType(kAudioServerPlugInTypeUUID);
    CHECK(factories && CFArrayGetCount(factories) > 0, "factory");
    IUnknownVTbl** unknown = CFPlugInInstanceCreate(NULL, (CFUUIDRef)CFArrayGetValueAtIndex(factories, 0), kAudioServerPlugInTypeUUID);
    CHECK(unknown, "instance");
    AudioServerPlugInDriverRef d = NULL;
    (*unknown)->QueryInterface(unknown, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), (LPVOID*)&d);
    CHECK(d, "interface");
    CHECK((*d)->Initialize(d, (AudioServerPlugInHostRef)1) == 0, "initialize");

    UInt32 size = 0;
    AudioObjectPropertyAddress a = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    AudioObjectID devices[4];
    size = sizeof(devices);
    CHECK((*d)->GetPropertyData(d, 1, 0, &a, 0, NULL, size, &size, devices) == 0 && size == 3 * sizeof(AudioObjectID), "device list");
    for (int i = 0; i < 3; i++) {
        CFStringRef name = NULL;
        a.mSelector = kAudioObjectPropertyName;
        CHECK((*d)->GetPropertyData(d, devices[i], 0, &a, 0, NULL, sizeof(name), &size, &name) == 0, "name");
        char buf[128];
        CFStringGetCString(name, buf, sizeof(buf), kCFStringEncodingUTF8);
        UInt32 hidden = 9;
        a.mSelector = kAudioDevicePropertyIsHidden;
        (*d)->GetPropertyData(d, devices[i], 0, &a, 0, NULL, sizeof(hidden), &size, &hidden);
        AudioObjectID streams[2];
        a.mSelector = kAudioDevicePropertyStreams;
        a.mScope = i == 0 ? kAudioObjectPropertyScopeInput : kAudioObjectPropertyScopeOutput;
        (*d)->GetPropertyDataSize(d, devices[i], 0, &a, 0, NULL, &size);
        UInt32 n = size / sizeof(AudioObjectID);
        (*d)->GetPropertyData(d, devices[i], 0, &a, 0, NULL, sizeof(streams), &size, streams);
        AudioStreamBasicDescription f;
        AudioObjectPropertyAddress fa = {kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
        (*d)->GetPropertyData(d, streams[0], 0, &fa, 0, NULL, sizeof(f), &size, &f);
        printf("device %u: %s, hidden %u, %u stream(s) in its scope, %.0f Hz %u ch\n", devices[i], buf, hidden, n, f.mSampleRate, f.mChannelsPerFrame);
        a.mScope = kAudioObjectPropertyScopeGlobal;
    }
    // UID lookup.
    a.mSelector = kAudioPlugInPropertyTranslateUIDToDevice;
    CFStringRef uid = CFSTR("io.github.aaron-gh.portputer.microphone-feed");
    AudioObjectID found = 0;
    (*d)->GetPropertyData(d, 1, 0, &a, sizeof(uid), &uid, sizeof(found), &size, &found);
    CHECK(found == devices[1], "UID lookup");

    // IO: feed at sample times 0..511, then the microphone reads them back.
    CHECK((*d)->StartIO(d, devices[0], 0) == 0 && (*d)->StartIO(d, devices[1], 0) == 0, "start");
    Float64 st; UInt64 ht, seed;
    CHECK((*d)->GetZeroTimeStamp(d, devices[0], 0, &st, &ht, &seed) == 0, "zero timestamp");
    Float32 out[512], in[512];
    for (int i = 0; i < 512; i++) out[i] = i / 512.0f;
    AudioServerPlugInIOCycleInfo cycle = {0};
    cycle.mOutputTime.mSampleTime = 0;
    cycle.mInputTime.mSampleTime = 0;
    (*d)->DoIOOperation(d, devices[1], 5, 0, kAudioServerPlugInIOOperationWriteMix, 512, &cycle, out, NULL);
    (*d)->DoIOOperation(d, devices[0], 3, 0, kAudioServerPlugInIOOperationReadInput, 512, &cycle, in, NULL);
    CHECK(in[100] == out[100] && in[511] == out[511], "loopback");
    cycle.mInputTime.mSampleTime = 512;
    (*d)->DoIOOperation(d, devices[0], 3, 0, kAudioServerPlugInIOOperationReadInput, 512, &cycle, in, NULL);
    CHECK(in[0] == 0 && in[511] == 0, "silence where nothing was written");
    (*d)->StopIO(d, devices[0], 0);
    (*d)->StopIO(d, devices[1], 0);
    // The speakers: stereo, 44.1 or 48 kHz, and they take what's played.
    a.mSelector = kAudioDevicePropertyAvailableNominalSampleRates;
    a.mScope = kAudioObjectPropertyScopeGlobal;
    AudioValueRange rates[4];
    CHECK((*d)->GetPropertyData(d, devices[2], 0, &a, 0, NULL, sizeof(rates), &size, rates) == 0 && size == 2 * sizeof(AudioValueRange), "speaker rates");
    CHECK(rates[0].mMinimum == 44100 && rates[1].mMinimum == 48000, "speaker rate values");
    a.mSelector = kAudioDevicePropertyDeviceCanBeDefaultDevice;
    UInt32 canDefault = 0;
    (*d)->GetPropertyData(d, devices[2], 0, &a, 0, NULL, sizeof(canDefault), &size, &canDefault);
    CHECK(canDefault == 1, "speakers can be the default output");
    CHECK((*d)->StartIO(d, devices[2], 0) == 0, "speakers start");
    Boolean willDo = false, inPlace = false;
    (*d)->WillDoIOOperation(d, devices[2], 0, kAudioServerPlugInIOOperationWriteMix, &willDo, &inPlace);
    CHECK(willDo, "speakers take the mix");
    Float32 stereo[1024];
    for (int i = 0; i < 1024; i++) stereo[i] = 0.5f;
    CHECK((*d)->DoIOOperation(d, devices[2], 7, 0, kAudioServerPlugInIOOperationWriteMix, 512, &cycle, stereo, NULL) == 0, "speakers write");
    (*d)->StopIO(d, devices[2], 0);
    printf("OK\n");
    return 0;
}
