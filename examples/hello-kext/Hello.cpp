// Hello, an example kext for Ambrosia.
//
// When the kernel loads the kext it calls the start routine of its
// kmod_info, which logs "loaded". The kext's one IOKit personality matches
// IOResources, so the kernel then makes an instance of
// ai_aneesiqbal_ambrosia_example_Hello and calls its start(), which logs a
// greeting and publishes it as the instance's "Greeting" property in the
// I/O Registry. From the guest:
//
//   kmutil showloaded --bundle-identifier ai.aneesiqbal.ambrosia.example.hello
//   ioreg -r -c ai_aneesiqbal_ambrosia_example_Hello -k Greeting
//
// It is built against the macOS SDK's Kernel.framework, with no Kernel
// Debug Kit: build.sh says how.

#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <mach/kmod.h>

// The class name is global to the kernel, so it carries the bundle
// identifier, as Apple's naming convention for IOKit classes asks.
class ai_aneesiqbal_ambrosia_example_Hello : public IOService {
    OSDeclareDefaultStructors(ai_aneesiqbal_ambrosia_example_Hello)

public:
    bool start(IOService *provider) override;
};

OSDefineMetaClassAndStructors(ai_aneesiqbal_ambrosia_example_Hello, IOService)

bool ai_aneesiqbal_ambrosia_example_Hello::start(IOService *provider) {
    if (!IOService::start(provider))
        return false;
    IOLog("ai.aneesiqbal.ambrosia.example.hello: Hello, world\n");
    setProperty("Greeting", "Hello, world");
    registerService();
    return true;
}

// The kext's kmod_info: its name, version, and the routines the kernel
// calls when it loads and unloads the kext. Xcode generates this for a kext
// target; without Xcode the kext declares it itself. Ambrosia finds it by
// its symbol, _kmod_info, and tells the kernel where it is.
extern "C" {
static kern_return_t hello_start(kmod_info_t *, void *) {
    IOLog("ai.aneesiqbal.ambrosia.example.hello: loaded\n");
    return KERN_SUCCESS;
}

static kern_return_t hello_stop(kmod_info_t *, void *) {
    return KERN_SUCCESS;
}

__attribute__((visibility("default")))
KMOD_EXPLICIT_DECL(ai.aneesiqbal.ambrosia.example.hello, "1.0.0", hello_start, hello_stop)
}
