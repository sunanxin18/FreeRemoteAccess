#
# To learn more about a Podspec see http://guides.cocoapods.org/syntax/podspec.html.
# Run `pod lib lint freeremote_native.podspec` to validate before publishing.
#
Pod::Spec.new do |s|
  s.name             = 'freeremote_native'
  s.version          = '0.1.0'
  s.summary          = 'FreeRemoteAccess Rust FFI bridge.'
  s.description      = <<-DESC
Versioned C ABI bridge for the FreeRemoteAccess Rust protocol core.
                       DESC
  s.homepage         = 'https://github.com/sunanxin18/FreeRemoteAccess'
  s.license          = { :type => 'Proprietary' }
  s.author           = { 'FreeRemoteAccess contributors' => 'noreply@localhost' }

  # This will ensure the source files in Classes/ are included in the native
  # builds of apps using this FFI plugin. Podspec does not support relative
  # paths, so Classes contains a forwarder C file that relatively imports
  # `../src/*` so that the C sources can be shared among all target platforms.
  s.source           = { :path => '.' }
  s.source_files = 'Classes/**/*'
  s.dependency 'Flutter'
  s.platform = :ios, '15.0'

  s.script_phase = {
    :name => 'Build FreeRemoteAccess Rust bridge',
    :script => '/bin/bash "${PODS_TARGET_SRCROOT}/../tool/build-apple.sh" ios "${CONFIGURATION}" "${ARCHS}" "${BUILT_PRODUCTS_DIR}/libfreeremote_native.a"',
    :execution_position => :before_compile,
    :output_files => ['${BUILT_PRODUCTS_DIR}/libfreeremote_native.a']
  }

  # Flutter.framework does not contain a i386 slice.
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386',
    'OTHER_LDFLAGS' => '$(inherited) -force_load "$(BUILT_PRODUCTS_DIR)/libfreeremote_native.a"'
  }
  s.swift_version = '5.0'
end
